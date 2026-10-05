//! Драйвер `simple-framebuffer`. Экспортирует `DisplayProvider` для узлов с совместимостью
//! `simple-framebuffer` и обслуживает сессии `Display` открытых экранов.
//!
//! Окно буфера кадра драйвер получает через `Platform` при открытии экрана и отображает с
//! профилем WriteCombining. Сессии провайдера и экранов обслуживает один порт, и значок кадра
//! называет его сессию.

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
use display_abi::{Area, DisplayDimension, DisplayError, display, display_provider};
use driver_simplefb::{Framebuffer, Refusal};
use ipc::{
    IntoWireValue, Listener, RecvError,
    wire::{Cap, MESSAGE_INLINE_MAX, Str},
};
use kernel_objects::{
    Delivery, Mapping, MemoryRegion, MmioMapping, MmioWindow, Port, ReadOnlyMapping, Resource,
    SessionTransport,
};
use platform_abi::{DEVICE_ID_MAX, Description, DeviceId, PlatformError, platform};
use runtime::{
    MemoryAccess, Mutex, PortTransport, Priority, Timeout, UserMemFlags, log_fmt, log_line,
    process_exit, spawn_owned, start_component,
};
use syscall::{Handle, PAGE_SIZE, Rights, kind};

const LOG_TAG: &str = "simplefb";

const EXPORT_STACK_SIZE: u64 = 0x4000;

component_manifest! {
    struct SimplefbStartup {
        requirements: { platform: session(platform::Protocol) },
        exports: [display_provider::Protocol => [{ compatible = "simple-framebuffer" }]],
    }
}

struct Sessions {
    service: Port,
    next_badge: AtomicU64,
    providers: Mutex<Vec<u64>>,
}

struct Driver {
    sessions: &'static Sessions,
    platform: platform::Client<SessionTransport>,
    description: ReadOnlyMapping,
    description_offset: u64,
    _description_region: MemoryRegion,
    screens: Vec<Screen>,
}

struct Screen {
    badge: u64,
    framebuffer: Framebuffer,
    pixels: MmioMapping,
    _window: MmioWindow,
    surface: Option<Surface>,
}

struct Surface {
    mapping: Mapping,
    _region: MemoryRegion,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OpenRefusal {
    InvalidDevice,
    Framebuffer(Refusal),
    Platform(PlatformError),
    PlatformCall,
    Map,
}

impl Driver {
    fn open(&mut self, device: &str) -> Result<Port, DisplayError> {
        let (framebuffer, window, pixels) = self.attach(device).map_err(|refusal| {
            log_fmt(format_args!("{device} refused: {refusal:?}"));
            DisplayError::Refused
        })?;
        let DisplayDimension { width, height } = framebuffer.dimension();
        log_fmt(format_args!(
            "{device} attached: {width}x{height}, stride {}, format {}",
            framebuffer.stride(),
            framebuffer.format()
        ));
        let MintedSession { badge, port } =
            mint_session(&self.sessions.service, &self.sessions.next_badge)
                .map_err(|_| DisplayError::NoSpace)?;

        // Запись держит окно до конца сессии, и закрытие окна отзывает выведенные из него страницы.
        self.screens.push(Screen {
            badge: badge.get(),
            framebuffer,
            pixels,
            _window: window,
            surface: None,
        });
        Ok(port)
    }

    fn attach(&self, device: &str) -> Result<(Framebuffer, MmioWindow, MmioMapping), OpenRefusal> {
        let id = DeviceId::new(device).ok_or(OpenRefusal::InvalidDevice)?;
        let description = Description::new(self.description.as_bytes(), self.description_offset)
            .map_err(|error| OpenRefusal::Framebuffer(error.into()))?;
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
        Ok((framebuffer, window, pixels))
    }

    fn close(&mut self, badge: u64) {
        self.screens.retain(|screen| screen.badge != badge);
        self.sessions
            .providers
            .lock()
            .retain(|&provider| provider != badge);
    }
}

impl display_provider::Service<Delivery> for Driver {
    fn open(
        &mut self,
        _frame: &Delivery,
        device: Str<'_, DEVICE_ID_MAX>,
    ) -> impl IntoWireValue<Delivery, Read = Result<ipc::wire::Session<display::Protocol>, DisplayError>>
    {
        Driver::open(self, device.as_str()).map(kernel_objects::Session::from_handle)
    }
}

impl Screen {
    fn acquire(&mut self) -> Result<MemoryRegion, DisplayError> {
        if self.surface.is_some() {
            return Err(DisplayError::AlreadyAcquired);
        }

        // Регион создаётся целыми страницами, а размер поверхности бывает не кратен странице.
        let size = self
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
        self.framebuffer.present(
            area,
            |offset, pixels| surface.mapping.copy_out_at(offset, pixels),
            |offset, pixels| self.pixels.copy_in_at(offset, pixels),
        )
    }
}

impl display::Service<Delivery> for Screen {
    fn dimension(&mut self, _frame: &Delivery) -> DisplayDimension {
        self.framebuffer.dimension()
    }

    fn acquire_surface(
        &mut self,
        _frame: &Delivery,
    ) -> impl IntoWireValue<Delivery, Read = Result<Cap<kind::MemoryRegion>, DisplayError>> {
        self.acquire()
    }

    fn present(&mut self, _frame: &Delivery, area: Area) -> Result<(), DisplayError> {
        Screen::present(self, area)
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn _start(parent: Handle<kind::Port>) -> ! {
    start_component(parent, LOG_TAG);
    let Some(mut driver) = start(parent) else {
        process_exit(1)
    };
    log_line("started");
    serve(&mut driver);
    process_exit(1)
}

fn start(parent: Handle<kind::Port>) -> Option<Driver> {
    let requirements = SimplefbStartup::request(parent)
        .inspect_err(|error| log_fmt(format_args!("{error}")))
        .ok()?;
    let platform = requirements.platform?.client();
    let description = platform.description().ok()?.ok()?;
    let description_offset = description.offset;
    // Драйвер держит регион описания до выхода, и закрытие хендла региона сняло бы отображение.
    let description_region = MemoryRegion::from(description.region);
    let description = description_region.map_full_read_only().ok()?;

    let sessions: &'static Sessions = Box::leak(Box::new(Sessions {
        service: Port::create().ok()?,
        next_badge: AtomicU64::new(0),
        providers: Mutex::new(Vec::new()),
    }));
    // Поток экспорта работает до конца процесса.
    let exporter = requirements.parent;
    spawn_owned(
        move || {
            let failure =
                serve_export::<display_provider::Protocol>(&exporter, || provider(sessions));
            log_fmt(format_args!("export stopped: {failure:?}"));
            process_exit(1)
        },
        EXPORT_STACK_SIZE,
        Priority::new(1),
    )
    .ok()?;

    Some(Driver {
        sessions,
        platform,
        description,
        description_offset,
        _description_region: description_region,
        screens: Vec::new(),
    })
}

fn provider(sessions: &Sessions) -> Result<Port, SessionError> {
    let MintedSession { badge, port } =
        mint_session(&sessions.service, &sessions.next_badge).map_err(|_| SessionError::NoSpace)?;
    sessions.providers.lock().push(badge.get());
    Ok(port)
}

fn serve(driver: &mut Driver) {
    let sessions = driver.sessions;
    let transport = PortTransport::new(sessions.service.borrow());
    let mut bytes = [0u8; MESSAGE_INLINE_MAX];
    loop {
        match transport.recv(&mut bytes, Timeout::INFINITE) {
            Ok(frame) => {
                let badge = frame.resources.badge;
                if let Some(screen) = driver
                    .screens
                    .iter_mut()
                    .find(|screen| screen.badge == badge)
                {
                    let _ = display::dispatch(screen, frame);
                } else if sessions.providers.lock().contains(&badge) {
                    let _ = display_provider::dispatch(driver, frame);
                }
                // Кадр значка без сессии отвергается закрытием его ответа.
            }
            Err(RecvError::SenderGone(badge)) => driver.close(badge),
            Err(RecvError::Discarded | RecvError::Timeout) => {}
            Err(RecvError::Closed) => break,
        }
    }
}

#[panic_handler]
fn panic(info: &PanicInfo<'_>) -> ! {
    log_fmt(format_args!("panic: {info}"));
    process_exit(1)
}
