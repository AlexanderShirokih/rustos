//! Точка входа процесса: требования, поток экспорта `Display` и обслуживание сессий.
//!
//! Первая заявка привязывает GPU, уход последней сессии освобождает его.

use alloc::{boxed::Box, vec::Vec};
use core::fmt;

use component_startup::{
    MintedSession, SessionError, SessionEvent, SessionPort, StartupError, component_manifest,
    serve_export,
};
use display_abi::{Area, DisplayDimension, DisplayError, PhysicalSize, display};
use ipc::{IntoWireValue, wire::Cap};
use kernel_objects::{Delivery, Mapping, MemoryRegion, Port, Resource, SessionTransport};
use runtime::{MemoryAccess, Mutex, Timeout, UserMemFlags, log_fmt, log_line, spawn_bound};
use syscall::{PAGE_SIZE, Rights, kind};
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
    ServicePort,
    ExportThread,
    ServicePortClosed,
}

impl fmt::Display for Stopped {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Startup(error) => error.fmt(formatter),
            Self::ServicePort => formatter.write_str("service port refused"),
            Self::ExportThread => formatter.write_str("export thread refused"),
            Self::ServicePortClosed => formatter.write_str("service port closed"),
        }
    }
}

struct Shared {
    service: SessionPort,
    state: Mutex<State>,
}

#[derive(Default)]
struct State {
    gpu: Option<Gpu>,
    sessions: Vec<Session>,
}

struct Session {
    badge: u64,
    surface: Option<Surface>,
}

struct Surface {
    mapping: Mapping,
}

struct Served<'a> {
    gpu: &'a mut Gpu,
    session: &'a mut Session,
}

impl Shared {
    fn open(&self, bus: &virtio_mmio::Client<SessionTransport>) -> Result<Port, SessionError> {
        let mut state = self.state.lock();
        if state.gpu.is_none() {
            let gpu = Gpu::bind(bus).map_err(|refusal| {
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
            state.gpu = Some(gpu);
        }

        let Ok(MintedSession { badge, port }) = self.service.create_session() else {
            state.release_when_unused();
            return Err(SessionError::NoSpace);
        };
        state.sessions.push(Session {
            badge: badge.get(),
            surface: None,
        });
        Ok(port)
    }

    fn serve(&self) {
        let mut events = self.service.events();

        while let Some(event) = events.recv() {
            match event {
                SessionEvent::Frame(frame) => {
                    let badge = frame.resources.badge;
                    let mut state = self.state.lock();
                    let State { gpu, sessions } = &mut *state;
                    let session = sessions.iter_mut().find(|session| session.badge == badge);
                    // Кадр значка без сессии отвергается закрытием его ответа.
                    if let (Some(gpu), Some(session)) = (gpu.as_mut(), session) {
                        let _ = display::dispatch(&mut Served { gpu, session }, frame);
                    }
                }
                SessionEvent::Gone(badge) => {
                    let mut state = self.state.lock();
                    state.sessions.retain(|session| session.badge != badge);
                    state.release_when_unused();
                }
            }
        }
    }
}

impl State {
    fn release_when_unused(&mut self) {
        if self.sessions.is_empty()
            && let Some(gpu) = self.gpu.take()
        {
            gpu.release();
            log_line("gpu released");
        }
    }
}

impl Served<'_> {
    fn acquire(&mut self) -> Result<MemoryRegion, DisplayError> {
        if self.session.surface.is_some() {
            return Err(DisplayError::AlreadyAcquired);
        }

        // Регион создаётся целыми страницами, а размер поверхности бывает не кратен странице.
        let size = self
            .gpu
            .dimension()
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

        self.session.surface = Some(Surface { mapping });
        Ok(granted)
    }
}

impl display::Service<Delivery> for Served<'_> {
    fn dimension(&mut self, _frame: &Delivery) -> DisplayDimension {
        self.gpu.dimension()
    }

    fn physical_size(&mut self, _frame: &Delivery) -> Option<PhysicalSize> {
        self.gpu.physical_size()
    }

    fn acquire_surface(
        &mut self,
        _frame: &Delivery,
    ) -> impl IntoWireValue<Delivery, Read = Result<Cap<kind::MemoryRegion>, DisplayError>> {
        self.acquire()
    }

    fn present(&mut self, _frame: &Delivery, area: Area) -> Result<(), DisplayError> {
        let surface = self
            .session
            .surface
            .as_ref()
            .ok_or(DisplayError::NoSurface)?;
        self.gpu.present(&surface.mapping, area)
    }
}

#[program_entry::component]
fn run(requirements: VirtioGpuStartup) -> Result<(), Stopped> {
    let bus = requirements
        .virtio
        .open()
        .map_err(Stopped::Startup)?
        .client()
        .with_wait(BUS_CALL_TIMEOUT);
    let service = SessionPort::create().map_err(|_| Stopped::ServicePort)?;
    let shared: &'static Shared = Box::leak(Box::new(Shared {
        service,
        state: Mutex::new(State::default()),
    }));

    // Поток экспорта работает до конца процесса.
    let exporter = requirements.parent;
    let _ = spawn_bound(move || serve_export::<display::Protocol>(exporter, |_| shared.open(&bus)))
        .map_err(|_| Stopped::ExportThread)?;

    shared.serve();
    Err(Stopped::ServicePortClosed)
}
