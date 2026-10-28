//! Обслуживание диска и его сессии задачами одного executor.
//!
//! Первая заявка подключает диск. Новая заявка ждёт ухода прежней сессии до
//! [`SESSION_WAIT`]. Уход сессии отзывает её буфер, а активная DMA-операция завершает очистку.

use alloc::{
    rc::{Rc, Weak},
    vec::Vec,
};
use core::{cell::RefCell, fmt};

use block_abi::{BlockError, BlockInfo, SESSION_WAIT, block_device};
use component_startup::{
    AsyncStartup, MintedSession, Requirements as _, SessionError, SessionPort, StartupError,
    component_manifest, serve_component,
};
use driver_virtio_blk::{Disk, SessionBuffer};
use executor::{Executor, JoinHandle, Mutex, PortChannel};
use ipc::{IntoWireValue, ServiceOwner, wire::Cap};
use kernel_objects::{Delivery, MemoryRegion, Port, SessionTransport};
use runtime::{Instant, Timeout, log_fmt};
use syscall::{Handle, Rights, kind};
use virtio::block::RequestKind;
use virtio_driver::DmaRegion;
use virtio_mmio_abi::virtio_mmio;

use crate::device::Device;

const BUS_CALL_TIMEOUT: Timeout = Timeout::from_secs(2);
const BUSY_PAUSE: Timeout = Timeout::from_millis(5);

component_manifest! {
    struct VirtioBlockStartup {
        requirements: { virtio: session(virtio_mmio::Protocol) },
        features: ["virtio,mmio"],
        exports: [block_device::Protocol => [{ layout = "disk", disk = "virtio" }]],
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
    bound: Mutex<Option<Rc<Bound>>>,
    session: RefCell<Option<Rc<Session>>>,
    executor: Weak<Executor>,
    cleanups: RefCell<Vec<JoinHandle<()>>>,
    bus: virtio_mmio::Client<PortChannel<SessionTransport>>,
}

struct Bound {
    disk: Mutex<Disk<Device>>,
    info: BlockInfo,
}

struct Session {
    badge: u64,
    bound: Rc<Bound>,
    buffer: RefCell<Option<Granted>>,
}

struct Granted {
    buffer: Rc<SessionBuffer<DmaRegion>>,
    _bridge: MemoryRegion,
}

#[derive(Clone)]
struct Served {
    session: Rc<Session>,
    executor: Weak<Executor>,
}

impl ServiceOwner for Served {
    type Service = Self;

    fn service(&self) -> &Self {
        self
    }
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
        let start = Instant::now();
        while self.session.borrow().is_some() {
            if Instant::now().since(start) >= SESSION_WAIT {
                return Err(SessionError::Refused);
            }

            executor::sleep(BUSY_PAUSE).await;
        }

        let task = {
            let executor = self.executor.upgrade().ok_or(SessionError::Unavailable)?;
            let shared = self.clone();
            executor.spawn(async move {
                let mut bound = shared.bound.lock().await;
                if bound.is_none() {
                    let (device, info) = Device::connect(&shared.bus).await.map_err(|refusal| {
                        log_fmt(format_args!("block device refused: {refusal:?}"));
                        SessionError::Refused
                    })?;
                    let BlockInfo {
                        block_size, blocks, ..
                    } = info;
                    log_fmt(format_args!(
                        "block device attached: {blocks} blocks of {block_size} bytes"
                    ));
                    *bound = Some(Rc::new(Bound {
                        disk: Mutex::new(Disk::new(device, info)),
                        info,
                    }));
                }

                bound.clone().ok_or(SessionError::Unavailable)
            })
        };

        let bound = task.await.ok_or(SessionError::Unavailable)??;
        let MintedSession { badge, port } = self
            .service
            .create_session()
            .map_err(|_| SessionError::NoSpace)?;
        *self.session.borrow_mut() = Some(Rc::new(Session {
            badge: badge.get(),
            bound,
            buffer: RefCell::new(None),
        }));
        Ok(port)
    }

    fn gone(&self, badge: u64) {
        let session = self
            .session
            .borrow_mut()
            .take_if(|session| session.badge == badge);
        if let Some(session) = session {
            self.release(&session);
        }
    }

    fn release(&self, session: &Session) {
        let buffer = session
            .buffer
            .borrow_mut()
            .take()
            .map(|granted| granted.buffer);
        if let Some(buffer) = buffer
            && let Some(executor) = self.executor.upgrade()
        {
            let bound = session.bound.clone();
            let task = executor.spawn(async move {
                bound.disk.lock().await.release(buffer);
            });
            let mut cleanups = self.cleanups.borrow_mut();
            cleanups.retain(|task| !task.is_finished());
            cleanups.push(task);
        }
    }

    async fn stop(&self) {
        if let Some(session) = self.session.borrow_mut().take() {
            self.release(&session);
        }

        let cleanups = core::mem::take(&mut *self.cleanups.borrow_mut());
        for cleanup in cleanups {
            let _ = cleanup.await;
        }

        if let Some(bound) = self.bound.lock().await.take() {
            let stopped = bound.disk.lock().await.stop().await;
            if !stopped {
                core::mem::forget(bound);
            }
        }
    }
}

impl Served {
    fn acquire(&self, size: u32) -> Result<MemoryRegion, BlockError> {
        let mut buffer = self.session.buffer.borrow_mut();
        if buffer.is_some() {
            return Err(BlockError::AlreadyExists);
        }

        if size == 0 || size > self.session.bound.info.transfer_max {
            return Err(BlockError::NoSpace);
        }

        let region = DmaRegion::new(u64::from(size)).map_err(|_| BlockError::NoSpace)?;
        let rights = Rights::READ | Rights::WRITE | Rights::TRANSFER;
        let bridge = region
            .share(rights | Rights::DUPLICATE)
            .map_err(|_| BlockError::NoSpace)?;
        let granted = bridge
            .duplicate(rights, None)
            .map_err(|_| BlockError::NoSpace)?;
        *buffer = Some(Granted {
            buffer: Rc::new(SessionBuffer { region, size }),
            _bridge: bridge,
        });
        Ok(granted)
    }

    async fn transfer(&self, kind: RequestKind, lba: u64, count: u32) -> Result<(), BlockError> {
        let task = {
            let executor = self.executor.upgrade().ok_or(BlockError::Device)?;
            let session = Rc::downgrade(&self.session);
            let bound = self.session.bound.clone();
            let buffer = self
                .session
                .buffer
                .borrow()
                .as_ref()
                .map(|granted| granted.buffer.clone())
                .ok_or(BlockError::NoBuffer)?;

            executor.spawn(async move {
                let mut disk = bound.disk.lock().await;
                if session.upgrade().is_none() {
                    return Err(BlockError::Device);
                }

                disk.serve(kind, lba, count, Some(&buffer)).await
            })
        };

        task.await.ok_or(BlockError::Device)?
    }
}

#[allow(clippy::unused_async_trait_impl)]
impl block_device::Service<Delivery> for Served {
    fn info(&self, _frame: &Delivery) -> BlockInfo {
        self.session.bound.info
    }

    fn acquire_buffer(
        &self,
        _frame: &Delivery,
        size: u32,
    ) -> impl IntoWireValue<Delivery, Read = Result<Cap<kind::MemoryRegion>, BlockError>> {
        self.acquire(size)
    }

    async fn read(&self, _frame: &Delivery, lba: u64, count: u32) -> Result<(), BlockError> {
        self.transfer(RequestKind::Read, lba, count).await
    }

    async fn write(&self, _frame: &Delivery, lba: u64, count: u32) -> Result<(), BlockError> {
        self.transfer(RequestKind::Write, lba, count).await
    }
}

#[program_entry::component]
fn run(parent: Handle<kind::Port>) -> Result<(), Stopped> {
    let requirements = VirtioBlockStartup::from_parent(parent);
    let bus = requirements.virtio.open().map_err(Stopped::Startup)?;

    let executor = Rc::new(Executor::new().map_err(Stopped::Kernel)?);
    let service = Rc::new(SessionPort::create().map_err(Stopped::Kernel)?);
    let shared = Rc::new(Shared {
        service: service.clone(),
        bound: Mutex::new(None),
        session: RefCell::new(None),
        executor: Rc::downgrade(&executor),
        cleanups: RefCell::new(Vec::new()),
        bus: PortChannel::client(bus).with_wait(BUS_CALL_TIMEOUT),
    });

    let dispatch = shared.clone();
    let opener = shared.clone();
    let closer = shared.clone();

    let served = serve_component::<block_device::Protocol, _>(
        &executor,
        AsyncStartup::new(PortChannel::new(parent)),
        service,
        move |frame| {
            let session = dispatch
                .session
                .borrow()
                .as_ref()
                .filter(|session| session.badge == frame.resources.badge)
                .cloned();
            let session = session.ok_or(ipc::wire::IpcError::PeerClosed)?;
            let served = Served {
                session,
                executor: dispatch.executor.clone(),
            };
            block_device::serve(&served, frame)
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
