use alloc::{rc::Rc, vec::Vec};
use core::{cell::RefCell, fmt};

use component_startup::{
    AsyncStartup, MintedSession, Requirements as _, SessionError, SessionPort, StartupError,
    component_manifest, serve_component,
};
use display_abi::{Area, DisplayDimension, DisplayError, PhysicalSize, display};
use driver_simplefb::{Framebuffer, Refusal, SIMPLE_FRAMEBUFFER_COMPATIBLE};
use executor::{Executor, PortChannel};
use ipc::{
    IntoWireValue,
    wire::{Cap, IpcError},
};
use kernel_objects::{
    Delivery, Mapping, MemoryRegion, Port, Resource, SessionTransport, WriteCombiningMapping,
};
use platform_abi::{DeviceId, MappedDescription, PlatformError, platform};
use runtime::{MemoryAccess, UserMemFlags, log_fmt};
use syscall::{Handle, PAGE_SIZE, Rights, kind};

component_manifest! {
    struct SimplefbStartup {
        requirements: { platform: session(platform::Protocol) },
        features: ["simple-framebuffer"],
        exports: [display::Protocol],
    }
}

struct Shared {
    service: Rc<SessionPort>,
    sessions: RefCell<Vec<Rc<Session>>>,
}

struct Attacher {
    platform: platform::Client<PortChannel<SessionTransport>>,
    description: MappedDescription,
    screen: Option<Rc<Screen>>,
}

struct Screen {
    id: DeviceId,
    framebuffer: Framebuffer,
    pixels: WriteCombiningMapping,
}

struct Session {
    badge: u64,
    screen: Rc<Screen>,
    surface: RefCell<Option<Surface>>,
}

struct Surface {
    mapping: Mapping,
}

#[derive(Debug)]
enum Stopped {
    Startup(StartupError),
    Description,
    ServicePort,
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
            Self::ServicePortClosed => formatter.write_str("service port closed"),
        }
    }
}

impl Attacher {
    async fn open(&mut self, shared: &Shared) -> Result<Port, SessionError> {
        let screen = self.screen().await?;
        let MintedSession { badge, port } = shared
            .service
            .create_session()
            .map_err(|_| SessionError::NoSpace)?;

        shared.sessions.borrow_mut().push(Rc::new(Session {
            badge: badge.get(),
            screen,
            surface: RefCell::new(None),
        }));
        Ok(port)
    }

    async fn screen(&mut self) -> Result<Rc<Screen>, SessionError> {
        if let Some(screen) = &self.screen {
            return Ok(Rc::clone(screen));
        }

        let screen = self.attach().await.map_err(|refusal| {
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
        // Закрытие окна экрана отзывает выведенные из него страницы, поэтому `Attacher` держит
        // экран до конца процесса.
        Ok(Rc::clone(self.screen.insert(Rc::new(screen))))
    }

    async fn attach(&self) -> Result<Screen, OpenRefusal> {
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

        let window =
            self.platform.mmio(id.clone(), 0).await.map_err(|error| {
                error.fold(|_| OpenRefusal::PlatformCall, OpenRefusal::Platform)
            })?;

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

impl Session {
    fn acquire(&self) -> Result<MemoryRegion, DisplayError> {
        let mut surface = self.surface.borrow_mut();
        if surface.is_some() {
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

        *surface = Some(Surface { mapping });
        Ok(granted)
    }

    fn present(&self, area: Area) -> Result<(), DisplayError> {
        let surface = self.surface.borrow();
        let surface = surface.as_ref().ok_or(DisplayError::NoSurface)?;
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

#[allow(clippy::unused_async_trait_impl)]
impl display::Service<Delivery> for Session {
    fn dimension(&self, _frame: &Delivery) -> DisplayDimension {
        self.screen.framebuffer.dimension()
    }

    fn physical_size(&self, _frame: &Delivery) -> Option<PhysicalSize> {
        self.screen.framebuffer.physical_size()
    }

    fn acquire_surface(
        &self,
        _frame: &Delivery,
    ) -> impl IntoWireValue<Delivery, Read = Result<Cap<kind::MemoryRegion>, DisplayError>> {
        self.acquire()
    }

    async fn present(&self, _frame: &Delivery, area: Area) -> Result<(), DisplayError> {
        Session::present(self, area)
    }
}

#[program_entry::component]
fn run(parent: Handle<kind::Port>) -> Result<(), Stopped> {
    let requirements = SimplefbStartup::from_parent(parent);
    let platform_session = requirements.platform.open().map_err(Stopped::Startup)?;

    let executor = Rc::new(Executor::new().map_err(|_| Stopped::ServicePort)?);

    let platform = PortChannel::client(platform_session);

    let description = platform
        .description()
        .wait()
        .map_err(|_| Stopped::Description)?;

    let description = MappedDescription::new(description).map_err(|_| Stopped::Description)?;

    let mut attacher = Attacher {
        platform,
        description,
        screen: None,
    };

    let shared = Rc::new(Shared {
        service: Rc::new(SessionPort::create().map_err(|_| Stopped::ServicePort)?),
        sessions: RefCell::new(Vec::new()),
    });

    let open = shared.clone();
    let gone = shared.clone();

    let service = shared.service.clone();

    let served = serve_component::<display::Protocol, _>(
        &executor,
        AsyncStartup::new(PortChannel::new(parent)),
        service,
        move |frame| {
            let badge = frame.resources.badge;
            let session = shared
                .sessions
                .borrow()
                .iter()
                .find(|session| session.badge == badge)
                .cloned();
            let session = session.ok_or(IpcError::PeerClosed)?;
            display::serve(&session, frame)
        },
        async move |_| attacher.open(&open).await,
        move |badge| {
            gone.sessions
                .borrow_mut()
                .retain(|session| session.badge != badge);
        },
    );

    let _ = executor.block_on(served);
    Err(Stopped::ServicePortClosed)
}
