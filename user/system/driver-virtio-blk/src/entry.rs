//! Точка входа процесса: поток экспорта `BlockDevice` и обслуживание сессии диска.
//!
//! Первая заявка подключает диск. Диск обслуживает одну сессию, и заявка ждёт ухода
//! прежней сессии до [`SESSION_WAIT`].

use alloc::boxed::Box;
use core::{fmt, time::Duration};

use block_abi::{BlockError, BlockInfo, SESSION_WAIT, block_device};
use component_startup::{
    MintedSession, SessionError, SessionEvent, SessionPort, StartupError, component_manifest,
    serve_export,
};
use driver_virtio_blk::{Disk, SessionBuffer};
use ipc::{IntoWireValue, wire::Cap};
use kernel_objects::{Delivery, MemoryRegion, Port, SessionTransport};
use runtime::{Instant, Mutex, Timeout, log_fmt, spawn_bound};
use syscall::{Rights, kind};
use virtio::block::RequestKind;
use virtio_driver::DmaRegion;
use virtio_mmio_abi::virtio_mmio;

use crate::device::Device;

const BUS_CALL_TIMEOUT: Timeout = Timeout::from_secs(2);

const BUSY_PAUSE: Duration = Duration::from_millis(5);

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
    ServicePort,
    ExportThread,
    ServicePortClosed,
}

struct Shared {
    service: SessionPort,
    state: Mutex<State>,
}

#[derive(Default)]
struct State {
    disk: Option<Disk<Device>>,
    session: Option<Session>,
}

struct Session {
    badge: u64,
    buffer: Option<Granted>,
}

struct Granted {
    buffer: SessionBuffer<DmaRegion>,
    bridge: MemoryRegion,
}

struct Served<'a> {
    disk: &'a mut Disk<Device>,
    buffer: &'a mut Option<Granted>,
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

impl Shared {
    fn open(&self, bus: &virtio_mmio::Client<SessionTransport>) -> Result<Port, SessionError> {
        let start = Instant::now();
        loop {
            {
                let mut state = self.state.lock();
                if state.session.is_none() {
                    if state.disk.is_none() {
                        state.disk = Some(connect(bus)?);
                    }
                    let MintedSession { badge, port } = self
                        .service
                        .create_session()
                        .map_err(|_| SessionError::NoSpace)?;
                    state.session = Some(Session {
                        badge: badge.get(),
                        buffer: None,
                    });
                    return Ok(port);
                }
            }
            if Instant::now().since(start) >= SESSION_WAIT {
                return Err(SessionError::Refused);
            }
            runtime::sleep(BUSY_PAUSE);
        }
    }

    fn serve(&self) {
        let mut events = self.service.events();

        while let Some(event) = events.recv() {
            let mut state = self.state.lock();
            match event {
                SessionEvent::Frame(frame) => {
                    let badge = frame.resources.badge;
                    let State { disk, session } = &mut *state;
                    let session = session.as_mut().filter(|session| session.badge == badge);
                    // Кадр значка без сессии отвергается закрытием его ответа.
                    if let (Some(disk), Some(session)) = (disk.as_mut(), session) {
                        let buffer = &mut session.buffer;
                        let _ = block_device::dispatch(&mut Served { disk, buffer }, frame);
                    }
                }
                SessionEvent::Gone(badge) => {
                    if let Some(Session {
                        buffer: Some(Granted { buffer, bridge }),
                        ..
                    }) = state.session.take_if(|session| session.badge == badge)
                    {
                        // Капа клиента производна от `bridge` и отзывается, даже если регион
                        // остаётся занятым.
                        drop(bridge);
                        if let Some(disk) = &state.disk {
                            disk.release(buffer.region);
                        }
                    }
                }
            }
        }
    }
}

impl Served<'_> {
    fn acquire(&mut self, size: u32) -> Result<MemoryRegion, BlockError> {
        if self.buffer.is_some() {
            return Err(BlockError::AlreadyExists);
        }
        if size == 0 || size > self.disk.info().transfer_max {
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
        *self.buffer = Some(Granted {
            buffer: SessionBuffer { region, size },
            bridge,
        });
        Ok(granted)
    }

    fn transfer(&mut self, kind: RequestKind, lba: u64, count: u32) -> Result<(), BlockError> {
        let buffer = self.buffer.as_ref().map(|granted| &granted.buffer);
        self.disk.serve(kind, lba, count, buffer)
    }
}

impl block_device::Service<Delivery> for Served<'_> {
    fn info(&mut self, _frame: &Delivery) -> BlockInfo {
        self.disk.info()
    }

    fn acquire_buffer(
        &mut self,
        _frame: &Delivery,
        size: u32,
    ) -> impl IntoWireValue<Delivery, Read = Result<Cap<kind::MemoryRegion>, BlockError>> {
        self.acquire(size)
    }

    fn read(&mut self, _frame: &Delivery, lba: u64, count: u32) -> Result<(), BlockError> {
        self.transfer(RequestKind::Read, lba, count)
    }

    fn write(&mut self, _frame: &Delivery, lba: u64, count: u32) -> Result<(), BlockError> {
        self.transfer(RequestKind::Write, lba, count)
    }
}

#[program_entry::component]
fn run(requirements: VirtioBlockStartup) -> Result<(), Stopped> {
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
    let _ = spawn_bound(move || {
        serve_export::<block_device::Protocol>(exporter, |_| shared.open(&bus))
    })
    .map_err(|_| Stopped::ExportThread)?;

    shared.serve();
    Err(Stopped::ServicePortClosed)
}

fn connect(bus: &virtio_mmio::Client<SessionTransport>) -> Result<Disk<Device>, SessionError> {
    let (device, info) = Device::connect(bus).map_err(|refusal| {
        log_fmt(format_args!("block device refused: {refusal:?}"));
        SessionError::Refused
    })?;
    let BlockInfo {
        block_size, blocks, ..
    } = info;
    log_fmt(format_args!(
        "block device attached: {blocks} blocks of {block_size} bytes"
    ));
    Ok(Disk::new(device, info))
}
