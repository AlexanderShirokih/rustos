//! Драйвер буфера кадра `simple-framebuffer`. Предоставляет протокол `Display`: выдаёт сессии
//! поверхность для рисования и по `present` переносит её прямоугольник в буфер кадра в формате
//! пикселей устройства. Буфер кадра драйвер не настраивает: это должен сделать загрузчик до
//! старта системы.

#![no_std]
#![no_main]
#![allow(unsafe_code)]

runtime::process_heap!();

extern crate alloc;

use alloc::{boxed::Box, vec::Vec};
use core::{panic::PanicInfo, sync::atomic::AtomicU64};

use component_startup::{
    MintedSession, Requirements as _, SessionError, component_manifest, mint_session, serve_export,
};
use display_abi::{Area, DisplayDimension, DisplayError, display};
use driver_simplefb::{Framebuffer, Refusal, SIMPLE_FRAMEBUFFER_COMPATIBLE};
use ipc::{
    IntoWireValue, Listener, RecvError,
    wire::{Cap, MESSAGE_INLINE_MAX},
};
use kernel_objects::{
    Delivery, Mapping, MemoryRegion, MmioMapping, MmioWindow, Port, ReadOnlyMapping, Resource,
    SessionTransport,
};
use platform_abi::{Description, DeviceId, PlatformError, platform};
use runtime::{
    MemoryAccess, Mutex, PortTransport, Priority, Timeout, UserMemFlags, log_fmt, log_line,
    process_exit, spawn_owned, start_component,
};
use syscall::{Handle, PAGE_SIZE, Rights, kind};

const LOG_TAG: &str = "simplefb";

const EXPORT_THREAD_STACK_SIZE: u64 = 0x10000;

component_manifest! {
    struct SimplefbStartup {
        requirements: { platform: session(platform::Protocol) },
        features: ["simple-framebuffer"],
        exports: [display::Protocol],
    }
}

struct Shared {
    service: Port,
    sessions: Mutex<Vec<Session>>,
}

struct Attacher {
    platform: platform::Client<SessionTransport>,
    description: ReadOnlyMapping,
    description_offset: u64,
    _description_region: MemoryRegion,
    next_badge: AtomicU64,
    screen: Option<&'static Screen>,
}

struct Screen {
    id: DeviceId,
    framebuffer: Framebuffer,
    pixels: MmioMapping,
    _window: MmioWindow,
}

struct Session {
    badge: u64,
    screen: &'static Screen,
    surface: Option<Surface>,
}

struct Surface {
    mapping: Mapping,
    _region: MemoryRegion,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OpenRefusal {
    NoDevice,
    Framebuffer(Refusal),
    Platform(PlatformError),
    PlatformCall,
    Map,
}

impl Attacher {
    fn open(&mut self, shared: &Shared) -> Result<Port, SessionError> {
        let screen = self.screen()?;
        let MintedSession { badge, port } =
            mint_session(&shared.service, &self.next_badge).map_err(|_| SessionError::NoSpace)?;

        shared.sessions.lock().push(Session {
            badge: badge.get(),
            screen,
            surface: None,
        });
        Ok(port)
    }

    fn screen(&mut self) -> Result<&'static Screen, SessionError> {
        if let Some(screen) = self.screen {
            return Ok(screen);
        }

        let screen = self.attach().map_err(|refusal| {
            log_fmt(format_args!(
                "{SIMPLE_FRAMEBUFFER_COMPATIBLE} refused: {refusal:?}"
            ));
            SessionError::Refused
        })?;
        let framebuffer = screen.framebuffer;
        let DisplayDimension { width, height } = framebuffer.dimension();
        log_fmt(format_args!(
            "{} attached: {width}x{height}, stride {}, format {}",
            screen.id.as_str(),
            framebuffer.stride(),
            framebuffer.format()
        ));
        // Экран держит окно до конца процесса, и закрытие окна отзывает выведенные из него
        // страницы.
        Ok(*self.screen.insert(Box::leak(Box::new(screen))))
    }

    fn attach(&self) -> Result<Screen, OpenRefusal> {
        let description = Description::new(self.description.as_bytes(), self.description_offset)
            .map_err(|error| OpenRefusal::Framebuffer(error.into()))?;
        let id = description
            .devices()
            .filter_map(Result::ok)
            .find(|device| {
                device
                    .compatible()
                    .any(|compatible| compatible == SIMPLE_FRAMEBUFFER_COMPATIBLE)
            })
            .map(|device| device.id().clone())
            .ok_or(OpenRefusal::NoDevice)?;
        let framebuffer = Framebuffer::open(&description, &id).map_err(OpenRefusal::Framebuffer)?;

        let window = self
            .platform
            .mmio(id.as_wire(), 0)
            .map_err(|_| OpenRefusal::PlatformCall)?
            .map_err(OpenRefusal::Platform)?;
        framebuffer
            .fits(window.size)
            .map_err(OpenRefusal::Framebuffer)?;
        let window = MmioWindow {
            region: MemoryRegion::from(window.region),
            offset: window.offset,
            size: window.size,
        };
        let pixels = window.map_write_combining().map_err(|_| OpenRefusal::Map)?;
        Ok(Screen {
            id,
            framebuffer,
            pixels,
            _window: window,
        })
    }
}

impl Shared {
    fn serve(&self) {
        let transport = PortTransport::new(self.service.borrow());
        let mut bytes = [0u8; MESSAGE_INLINE_MAX];
        loop {
            match transport.recv(&mut bytes, Timeout::INFINITE) {
                Ok(frame) => {
                    let badge = frame.resources.badge;

                    if let Some(session) = self
                        .sessions
                        .lock()
                        .iter_mut()
                        .find(|session| session.badge == badge)
                    {
                        let _ = display::dispatch(session, frame);
                    }
                    // Кадр значка без сессии отвергается закрытием его ответа.
                }
                Err(RecvError::SenderGone(badge)) => {
                    self.sessions
                        .lock()
                        .retain(|session| session.badge != badge);
                }
                Err(RecvError::Discarded | RecvError::Timeout) => {}
                Err(RecvError::Closed) => break,
            }
        }
    }
}

impl Session {
    fn acquire(&mut self) -> Result<MemoryRegion, DisplayError> {
        if self.surface.is_some() {
            return Err(DisplayError::AlreadyAcquired);
        }

        // Регион создаётся целыми страницами, а размер поверхности бывает не кратен странице.
        let size = self
            .screen
            .framebuffer
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

        // Поле `mapping` объявлено в `Surface` раньше `_region` и снимается до закрытия региона.
        self.surface = Some(Surface {
            mapping,
            _region: region,
        });
        Ok(granted)
    }

    fn present(&self, area: Area) -> Result<(), DisplayError> {
        let surface = self.surface.as_ref().ok_or(DisplayError::NoSurface)?;
        self.screen.framebuffer.present(
            area,
            |offset, pixels| surface.mapping.copy_out_at(offset, pixels),
            |offset, pixels| self.screen.pixels.copy_in_at(offset, pixels),
        )
    }
}

impl display::Service<Delivery> for Session {
    fn dimension(&mut self, _frame: &Delivery) -> DisplayDimension {
        self.screen.framebuffer.dimension()
    }

    fn acquire_surface(
        &mut self,
        _frame: &Delivery,
    ) -> impl IntoWireValue<Delivery, Read = Result<Cap<kind::MemoryRegion>, DisplayError>> {
        self.acquire()
    }

    fn present(&mut self, _frame: &Delivery, area: Area) -> Result<(), DisplayError> {
        Session::present(self, area)
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn _start(parent: Handle<kind::Port>) -> ! {
    start_component(parent, LOG_TAG);
    let Some(shared) = start(parent) else {
        process_exit(1)
    };
    log_line("started");
    shared.serve();
    process_exit(1)
}

fn start(parent: Handle<kind::Port>) -> Option<&'static Shared> {
    let requirements = SimplefbStartup::request(parent)
        .inspect_err(|error| log_fmt(format_args!("{error}")))
        .ok()?;
    let platform = requirements.platform?.client();
    let description = platform.description().ok()?.ok()?;
    let description_offset = description.offset;
    // Драйвер держит регион описания до выхода, и закрытие хендла региона сняло бы отображение.
    let description_region = MemoryRegion::from(description.region);
    let description = description_region.map_full_read_only().ok()?;
    let mut attacher = Attacher {
        platform,
        description,
        description_offset,
        _description_region: description_region,
        next_badge: AtomicU64::new(0),
        screen: None,
    };

    let shared: &'static Shared = Box::leak(Box::new(Shared {
        service: Port::create().ok()?,
        sessions: Mutex::new(Vec::new()),
    }));
    // Поток экспорта работает до конца процесса.
    let exporter = requirements.parent;
    spawn_owned(
        move || {
            let failure = serve_export::<display::Protocol>(&exporter, || attacher.open(shared));
            log_fmt(format_args!("export stopped: {failure:?}"));
            process_exit(1)
        },
        EXPORT_THREAD_STACK_SIZE,
        Priority::new(1),
    )
    .ok()?;

    Some(shared)
}

#[panic_handler]
fn panic(info: &PanicInfo<'_>) -> ! {
    log_fmt(format_args!("panic: {info}"));
    process_exit(1)
}
