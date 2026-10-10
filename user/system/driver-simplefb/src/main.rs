//! Драйвер буфера кадра `simple-framebuffer`. Предоставляет протокол `Display`: выдаёт сессии
//! поверхность для рисования и по `present` переносит её прямоугольник в буфер кадра в формате
//! пикселей устройства. Буфер кадра драйвер не настраивает: это должен сделать загрузчик до
//! старта системы.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::{boxed::Box, vec::Vec};
use core::{fmt, sync::atomic::AtomicU64};

use component_startup::{
    MintedSession, SessionError, StartupError, component_manifest, mint_session, serve_export,
};
use display_abi::{Area, DisplayDimension, DisplayError, display};
use driver_simplefb::{Framebuffer, Refusal, SIMPLE_FRAMEBUFFER_COMPATIBLE};
use ipc::{
    IntoWireValue, Listener, RecvError,
    wire::{Cap, MESSAGE_INLINE_MAX},
};
use kernel_objects::{
    Delivery, Mapping, MemoryRegion, Port, Resource, SessionTransport, WriteCombiningMapping,
};
use platform_abi::{DeviceId, MappedDescription, PlatformError, platform};
use runtime::{
    FAILED_EXIT_CODE, MemoryAccess, Mutex, PortTransport, Timeout, UserMemFlags, exit, log_fmt,
    spawn_owned,
};
use syscall::{PAGE_SIZE, Rights, kind};

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
    description: MappedDescription,
    next_badge: AtomicU64,
    screen: Option<&'static Screen>,
}

struct Screen {
    id: DeviceId,
    framebuffer: Framebuffer,
    pixels: WriteCombiningMapping,
}

struct Session {
    badge: u64,
    screen: &'static Screen,
    surface: Option<Surface>,
}

struct Surface {
    mapping: Mapping,
}

#[derive(Debug)]
enum Stopped {
    Startup(StartupError),
    Description,
    ServicePort,
    ExportThread,
    ServicePortClosed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OpenRefusal {
    NoDevice,
    Framebuffer(Refusal),
    Platform(PlatformError),
    PlatformCall,
    Map,
}

impl fmt::Display for Stopped {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Startup(error) => error.fmt(formatter),
            Self::Description => formatter.write_str("platform description refused"),
            Self::ServicePort => formatter.write_str("service port refused"),
            Self::ExportThread => formatter.write_str("export thread refused"),
            Self::ServicePortClosed => formatter.write_str("service port closed"),
        }
    }
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
        let description = self
            .description
            .description()
            .map_err(|error| OpenRefusal::Framebuffer(error.into()))?;
        let id = description
            .compatible_devices(SIMPLE_FRAMEBUFFER_COMPATIBLE)
            .next()
            .map(|device| device.id().clone())
            .ok_or(OpenRefusal::NoDevice)?;
        let framebuffer = Framebuffer::open(&description, &id).map_err(OpenRefusal::Framebuffer)?;

        let window = self
            .platform
            .mmio(id.as_str(), 0)
            .map_err(|error| error.fold(|_| OpenRefusal::PlatformCall, OpenRefusal::Platform))?;
        framebuffer
            .fits(window.size)
            .map_err(OpenRefusal::Framebuffer)?;
        let pixels = WriteCombiningMapping::new(window.into()).map_err(|_| OpenRefusal::Map)?;
        Ok(Screen {
            id,
            framebuffer,
            pixels,
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
        let mapping = Mapping::new(region, size, UserMemFlags::ReadWrite)
            .map_err(|_| DisplayError::NoSpace)?;
        let granted = mapping
            .region()
            .duplicate(Rights::READ | Rights::WRITE | Rights::TRANSFER, None)
            .map_err(|_| DisplayError::NoSpace)?;

        self.surface = Some(Surface { mapping });
        Ok(granted)
    }

    fn present(&self, area: Area) -> Result<(), DisplayError> {
        let surface = self.surface.as_ref().ok_or(DisplayError::NoSurface)?;
        self.screen.framebuffer.present(
            area,
            |offset, pixels| surface.mapping.copy_out_at(offset, pixels),
            |offset, pixels| self.screen.pixels.copy_in_at(offset, pixels),
            |surface_offset, screen_offset, len| {
                self.screen
                    .pixels
                    .copy_from(screen_offset, &surface.mapping, surface_offset, len)
            },
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

#[program_entry::component]
fn run(requirements: SimplefbStartup) -> Result<(), Stopped> {
    let shared = start(requirements)?;
    shared.serve();
    Err(Stopped::ServicePortClosed)
}

fn start(requirements: SimplefbStartup) -> Result<&'static Shared, Stopped> {
    let platform = requirements
        .platform
        .open()
        .map_err(Stopped::Startup)?
        .client();
    let description = platform.description().map_err(|_| Stopped::Description)?;
    let description = MappedDescription::new(description).map_err(|_| Stopped::Description)?;
    let mut attacher = Attacher {
        platform,
        description,
        next_badge: AtomicU64::new(0),
        screen: None,
    };

    let shared: &'static Shared = Box::leak(Box::new(Shared {
        service: Port::create().map_err(|_| Stopped::ServicePort)?,
        sessions: Mutex::new(Vec::new()),
    }));
    // Поток экспорта работает до конца процесса.
    let exporter = requirements.parent;
    let _ = spawn_owned(move || {
        let failure = serve_export::<display::Protocol>(&exporter, |_| attacher.open(shared));
        log_fmt(format_args!("export stopped: {failure:?}"));
        exit(FAILED_EXIT_CODE)
    })
    .map_err(|_| Stopped::ExportThread)?;

    Ok(shared)
}
