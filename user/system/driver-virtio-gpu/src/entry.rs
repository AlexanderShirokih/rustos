//! Обслуживание GPU и сессий дисплея задачами одного executor.
//!
//! Первая заявка привязывает GPU, уход последней сессии освобождает его.

use alloc::{
    rc::{Rc, Weak},
    vec::Vec,
};
use core::{cell::RefCell, fmt};

use component_startup::{
    AsyncStartup, MintedSession, Requirements as _, SessionError, SessionPort, StartupError,
    component_manifest, serve_component,
};
use display_abi::{Area, DisplayDimension, DisplayError, PhysicalSize, display};
use executor::{Executor, Mutex, PortChannel};
use ipc::{IntoWireValue, wire::Cap};
use kernel_objects::{Delivery, Mapping, MemoryRegion, Port, Resource, SessionTransport};
use runtime::{MemoryAccess, Timeout, UserMemFlags, log_fmt, log_line};
use syscall::{Handle, PAGE_SIZE, Rights, kind};
use virtio_mmio_abi::virtio_mmio;

use crate::device::Gpu;

const BUS_CALL_TIMEOUT: Timeout = Timeout::from_secs(2);

component_manifest! {
    struct VirtioGpuStartup {
        requirements: { virtio: session(virtio_mmio::Protocol) },
        features: ["virtio,mmio"],
        exports: [display::Protocol],
    }
}

#[derive(Debug)]
enum Stopped {
    Startup(StartupError),
    Kernel(kernel_objects::Error),
    ServicePortClosed,
}

struct Shared {
    service: Rc<SessionPort>,
    gpu: Mutex<Option<Gpu>>,
    sessions: RefCell<Vec<Rc<Session>>>,
    executor: Weak<Executor>,
    bus: virtio_mmio::Client<PortChannel<SessionTransport>>,
}

struct Session {
    badge: u64,
    dimension: DisplayDimension,
    physical_size: Option<PhysicalSize>,
    surface: RefCell<Option<Rc<Mapping>>>,
}

struct Served {
    shared: Rc<Shared>,
    session: Rc<Session>,
}

impl fmt::Display for Stopped {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Startup(error) => error.fmt(formatter),
            Self::Kernel(error) => error.fmt(formatter),
            Self::ServicePortClosed => formatter.write_str("service port closed"),
        }
    }
}

impl Shared {
    async fn open(self: &Rc<Self>) -> Result<Port, SessionError> {
        let task = {
            let executor = self.executor.upgrade().ok_or(SessionError::Unavailable)?;
            let shared = self.clone();
            executor.spawn(async move {
                let mut bound = shared.gpu.lock().await;
                if bound.is_none() {
                    let gpu = Gpu::bind(&shared.bus).await.map_err(|refusal| {
                        log_fmt(format_args!("gpu refused: {refusal:?}"));
                        SessionError::Refused
                    })?;
                    let DisplayDimension { width, height } = gpu.dimension();
                    match gpu.physical_size() {
                        Some(PhysicalSize {
                            width_mm,
                            height_mm,
                        }) => log_fmt(format_args!(
                            "gpu attached: {width}x{height}, {width_mm}x{height_mm} mm"
                        )),
                        None => log_fmt(format_args!(
                            "gpu attached: {width}x{height}, physical size unknown"
                        )),
                    }
                    *bound = Some(gpu);
                }

                let gpu = bound.as_ref().ok_or(SessionError::Unavailable)?;
                let dimension = gpu.dimension();
                let physical_size = gpu.physical_size();

                let minted = shared.service.create_session();
                let Ok(MintedSession { badge, port }) = minted else {
                    if shared.sessions.borrow().is_empty()
                        && let Some(gpu) = bound.take()
                    {
                        gpu.release().await;
                    }

                    return Err(SessionError::NoSpace);
                };

                shared.sessions.borrow_mut().push(Rc::new(Session {
                    badge: badge.get(),
                    dimension,
                    physical_size,
                    surface: RefCell::new(None),
                }));

                Ok(port)
            })
        };

        task.await.ok_or(SessionError::Unavailable)?
    }

    fn gone(self: &Rc<Self>, badge: u64) {
        self.sessions
            .borrow_mut()
            .retain(|session| session.badge != badge);

        self.release_when_unused();
    }

    fn release_when_unused(self: &Rc<Self>) {
        if self.sessions.borrow().is_empty()
            && let Some(executor) = self.executor.upgrade()
        {
            let shared = self.clone();
            executor.spawn(async move {
                let mut bound = shared.gpu.lock().await;
                if shared.sessions.borrow().is_empty()
                    && let Some(gpu) = bound.take()
                {
                    gpu.release().await;
                    log_line("gpu released");
                }
            });
        }
    }

    async fn stop(&self) {
        let mut bound = self.gpu.lock().await;

        self.sessions.borrow_mut().clear();
        if let Some(gpu) = bound.take() {
            gpu.release().await;
        }
    }
}

impl Served {
    fn acquire(&self) -> Result<MemoryRegion, DisplayError> {
        let mut surface = self.session.surface.borrow_mut();
        if surface.is_some() {
            return Err(DisplayError::AlreadyAcquired);
        }

        // Поверхность округляется до целых страниц региона.
        let size = self
            .session
            .dimension
            .surface_size()
            .and_then(|size| size.checked_next_multiple_of(PAGE_SIZE as u64))
            .ok_or(DisplayError::NoSpace)?;

        let region = Resource::self_resource()
            .and_then(|budget| budget.create_virtual(size, MemoryAccess::RW))
            .map_err(|_| DisplayError::NoSpace)?;
        let mapping = Mapping::new(region, size, UserMemFlags::ReadWrite)
            .map_err(|_| DisplayError::NoSpace)?;

        let granted = mapping
            .region()
            .duplicate(Rights::READ | Rights::WRITE | Rights::TRANSFER, None)
            .map_err(|_| DisplayError::NoSpace)?;

        *surface = Some(Rc::new(mapping));

        Ok(granted)
    }
}

#[allow(clippy::unused_async_trait_impl)]
impl display::Service<Delivery> for Served {
    fn dimension(&self, _frame: &Delivery) -> DisplayDimension {
        self.session.dimension
    }

    fn physical_size(&self, _frame: &Delivery) -> Option<PhysicalSize> {
        self.session.physical_size
    }

    fn acquire_surface(
        &self,
        _frame: &Delivery,
    ) -> impl IntoWireValue<Delivery, Read = Result<Cap<kind::MemoryRegion>, DisplayError>> {
        self.acquire()
    }

    async fn present(&self, _frame: &Delivery, area: Area) -> Result<(), DisplayError> {
        let surface = self
            .session
            .surface
            .borrow()
            .as_ref()
            .map(Rc::downgrade)
            .ok_or(DisplayError::NoSurface)?;
        self.session.dimension.check_area(area)?;
        let task = {
            let executor = self
                .shared
                .executor
                .upgrade()
                .ok_or(DisplayError::Hardware)?;
            let shared = self.shared.clone();
            executor.spawn(async move {
                let mut bound = shared.gpu.lock().await;
                if surface.upgrade().is_none() {
                    return Err(DisplayError::NoSurface);
                }
                bound
                    .as_mut()
                    .ok_or(DisplayError::Hardware)?
                    .present(&surface, area)
                    .await
            })
        };

        task.await.ok_or(DisplayError::Hardware)?
    }
}

#[program_entry::component]
fn run(parent: Handle<kind::Port>) -> Result<(), Stopped> {
    let requirements = VirtioGpuStartup::from_parent(parent);
    let bus = requirements.virtio.open().map_err(Stopped::Startup)?;

    let executor = Rc::new(Executor::new().map_err(Stopped::Kernel)?);
    let service = Rc::new(SessionPort::create().map_err(Stopped::Kernel)?);

    let shared = Rc::new(Shared {
        service: service.clone(),
        gpu: Mutex::new(None),
        sessions: RefCell::new(Vec::new()),
        executor: Rc::downgrade(&executor),
        bus: PortChannel::client(bus).with_wait(BUS_CALL_TIMEOUT),
    });

    let dispatch = shared.clone();
    let opener = shared.clone();
    let closer = shared.clone();

    let served = serve_component::<display::Protocol, _>(
        &executor,
        AsyncStartup::new(PortChannel::new(parent)),
        service,
        move |frame| {
            let session = dispatch
                .sessions
                .borrow()
                .iter()
                .find(|session| session.badge == frame.resources.badge)
                .cloned();
            let session = session.ok_or(ipc::wire::IpcError::PeerClosed)?;
            let served = Rc::new(Served {
                shared: dispatch.clone(),
                session,
            });
            display::serve(&served, frame)
        },
        async move |_| opener.open().await,
        move |badge| closer.gone(badge),
    );

    executor.block_on(async {
        let _ = served.await;
        shared.stop().await;
    });
    Err(Stopped::ServicePortClosed)
}
