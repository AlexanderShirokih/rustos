//! Точка входа процесса: требования, поток экспорта `Display` и обслуживание сессий.
//!
//! Первая заявка привязывает GPU, уход последней сессии освобождает его.

use alloc::{boxed::Box, vec::Vec};
use core::{panic::PanicInfo, sync::atomic::AtomicU64};

use component_startup::{
    MintedSession, Requirements as _, SessionError, component_manifest, mint_session, serve_export,
};
use display_abi::{Area, DisplayDimension, DisplayError, display};
use ipc::{
    IntoWireValue, Listener, RecvError,
    wire::{Cap, MESSAGE_INLINE_MAX},
};
use kernel_objects::{Delivery, Mapping, MemoryRegion, Port, Resource, SessionTransport};
use runtime::{
    MemoryAccess, Mutex, PortTransport, Priority, Timeout, UserMemFlags, log_fmt, log_line,
    process_exit, spawn_owned, start_component,
};
use syscall::{Handle, PAGE_SIZE, Rights, kind};
use virtio_mmio_abi::virtio_mmio;

use crate::device::Gpu;

const LOG_TAG: &str = "virtio-gpu";

const EXPORT_THREAD_STACK_SIZE: u64 = 0x10000;

const BUS_CALL_TIMEOUT: Timeout = Timeout::from_secs(2);

runtime::process_heap!();

component_manifest! {
    struct VirtioGpuStartup {
        requirements: { virtio: session(virtio_mmio::Protocol) },
        features: ["virtio,mmio"],
        exports: [display::Protocol],
    }
}

struct Shared {
    service: Port,
    next_badge: AtomicU64,
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
    _region: MemoryRegion,
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
            log_fmt(format_args!("gpu attached: {width}x{height}"));
            state.gpu = Some(gpu);
        }

        let Ok(MintedSession { badge, port }) = mint_session(&self.service, &self.next_badge)
        else {
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
        let transport = PortTransport::new(self.service.borrow());
        let mut bytes = [0u8; MESSAGE_INLINE_MAX];
        loop {
            match transport.recv(&mut bytes, Timeout::INFINITE) {
                Ok(frame) => {
                    let badge = frame.resources.badge;
                    let mut state = self.state.lock();
                    let State { gpu, sessions } = &mut *state;
                    let session = sessions.iter_mut().find(|session| session.badge == badge);
                    // Кадр значка без сессии отвергается закрытием его ответа.
                    if let (Some(gpu), Some(session)) = (gpu.as_mut(), session) {
                        let _ = display::dispatch(&mut Served { gpu, session }, frame);
                    }
                }
                Err(RecvError::SenderGone(badge)) => {
                    let mut state = self.state.lock();
                    state.sessions.retain(|session| session.badge != badge);
                    state.release_when_unused();
                }
                Err(RecvError::Discarded | RecvError::Timeout) => {}
                Err(RecvError::Closed) => break,
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
        let mapping = region
            .map(size, UserMemFlags::ReadWrite)
            .map_err(|_| DisplayError::NoSpace)?;
        let granted = region
            .duplicate(Rights::READ | Rights::WRITE | Rights::TRANSFER, None)
            .map_err(|_| DisplayError::NoSpace)?;

        self.session.surface = Some(Surface {
            mapping,
            _region: region,
        });
        Ok(granted)
    }
}

impl display::Service<Delivery> for Served<'_> {
    fn dimension(&mut self, _frame: &Delivery) -> DisplayDimension {
        self.gpu.dimension()
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

#[allow(unsafe_code)]
#[unsafe(no_mangle)]
pub(crate) extern "C" fn _start(parent: Handle<kind::Port>) -> ! {
    start_component(parent, LOG_TAG);
    run(parent);
    process_exit(1)
}

fn run(parent: Handle<kind::Port>) {
    let requirements = match VirtioGpuStartup::request(parent) {
        Ok(requirements) => requirements,
        Err(error) => return log_fmt(format_args!("{error}")),
    };
    let Some(bus) = requirements.virtio else {
        return log_line("virtio-mmio session not granted");
    };
    let bus = bus.client().with_wait(BUS_CALL_TIMEOUT);
    let Ok(service) = Port::create() else {
        return log_line("service port refused");
    };
    let shared: &'static Shared = Box::leak(Box::new(Shared {
        service,
        next_badge: AtomicU64::new(0),
        state: Mutex::new(State::default()),
    }));

    // Поток экспорта работает до конца процесса.
    let exporter = requirements.parent;
    let export_thread = spawn_owned(
        move || {
            let failure = serve_export::<display::Protocol>(&exporter, || shared.open(&bus));
            log_fmt(format_args!("export stopped: {failure:?}"));
            process_exit(1)
        },
        EXPORT_THREAD_STACK_SIZE,
        Priority::new(1),
    );
    if export_thread.is_err() {
        return log_line("export thread refused");
    }

    log_line("started");
    shared.serve();
}

#[panic_handler]
fn panic(info: &PanicInfo<'_>) -> ! {
    log_fmt(format_args!("panic: {info}"));
    process_exit(1)
}
