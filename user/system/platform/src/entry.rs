use alloc::{boxed::Box, rc::Rc, vec::Vec};
use core::{cell::RefCell, fmt};

use bootstrap_abi::{features, hardware};
use component_startup::{
    AsyncStartup, MintedSession, Requirements as _, SessionError, SessionPort, StartupError,
    component_manifest, serve_component,
};
use device_resolver::{DeviceTreePlatform, PLATFORM_BINDINGS};
use executor::{Executor, PortChannel};
use fdt::devicetree::DeviceTree;
use kernel_objects::{IrqControl, MemoryRegion, Port, ReadOnlyMapping, SessionTransport};
use platform_abi::{Description, platform};
use runtime::log_fmt;
use syscall::{Handle, kind};

use crate::sessions::{HardwareDescription, Platform, SessionRecord};

component_manifest! {
    struct PlatformStartup {
        requirements: {
            hardware: session(hardware::Protocol),
            features: session(features::Protocol),
        },
        exports: [platform::Protocol],
    }
}

#[derive(Debug)]
enum Stopped {
    Startup(StartupError),
    Hardware,
    Description,
    ServicePort,
    Executor,
    ServicePortClosed,
}

struct Shared {
    sessions: Rc<RefCell<Vec<SessionRecord>>>,
    service: Rc<SessionPort>,
}

impl fmt::Display for Stopped {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Startup(error) => error.fmt(formatter),
            Self::Hardware => formatter.write_str("hardware refused"),
            Self::Description => formatter.write_str("platform description malformed"),
            Self::ServicePort => formatter.write_str("service port refused"),
            Self::Executor => formatter.write_str("executor refused"),
            Self::ServicePortClosed => formatter.write_str("service port closed"),
        }
    }
}

#[program_entry::component]
fn run(parent: Handle<kind::Port>) -> Result<(), Stopped> {
    let requirements = PlatformStartup::from_parent(parent);

    let executor = Rc::new(Executor::new().map_err(|_| Stopped::Executor)?);
    executor.block_on(async {
        let (shared, platform) = start(&requirements).await?;
        let platform = Rc::new(platform);
        let dispatch = platform.clone();
        let closed = platform.clone();
        let opened = shared.clone();

        let served = serve_component::<platform::Protocol, _>(
            &executor,
            AsyncStartup::new(PortChannel::new(parent)),
            shared.service.clone(),
            move |frame| platform::serve(&dispatch, frame),
            async move |_| open(&opened),
            move |badge| closed.close(badge),
        );
        let _ = served.await;
        Err(Stopped::ServicePortClosed)
    })
}

async fn start(requirements: &PlatformStartup) -> Result<(Rc<Shared>, Platform), Stopped> {
    let hardware = requirements
        .hardware
        .open_async()
        .await
        .map_err(Stopped::Startup)?;

    let hardware = PortChannel::client(hardware);
    let lines = IrqControl::from(
        hardware
            .acquire_irq_control()
            .await
            .map_err(|_| Stopped::Hardware)?,
    );

    let fdt = hardware
        .acquire_boot_fdt()
        .await
        .map_err(|_| Stopped::Hardware)?;

    let offset = usize::try_from(fdt.offset).map_err(|_| Stopped::Description)?;
    let mapping: &'static ReadOnlyMapping = Box::leak(Box::new(
        ReadOnlyMapping::new(MemoryRegion::from(fdt.region)).map_err(|_| Stopped::Description)?,
    ));
    let tree = mapping
        .as_bytes()
        .get(offset..)
        .and_then(|bytes| DeviceTree::from_bytes(bytes).ok())
        .ok_or(Stopped::Description)?;
    let tree: &'static DeviceTree<'static> = Box::leak(Box::new(tree));

    let shared = Rc::new(Shared {
        sessions: Rc::new(RefCell::new(Vec::new())),
        service: Rc::new(SessionPort::create().map_err(|_| Stopped::ServicePort)?),
    });

    let description = Description::new(mapping.as_bytes(), u64::from(fdt.offset))
        .map_err(|_| Stopped::Description)?;
    let features = requirements
        .features
        .open_async()
        .await
        .map_err(Stopped::Startup)?;

    let features = PortChannel::client(features);
    publish(&features, &description).await;

    Ok((
        shared.clone(),
        Platform::new(
            shared.sessions.clone(),
            DeviceTreePlatform::new(tree, PLATFORM_BINDINGS),
            HardwareDescription {
                region: mapping.region(),
                offset: u64::from(fdt.offset),
            },
            lines,
            hardware,
        ),
    ))
}

async fn publish(
    features: &features::Client<PortChannel<SessionTransport>>,
    description: &Description<'_>,
) {
    let mut published: Vec<&str> = Vec::new();
    for device in description.devices().filter_map(Result::ok) {
        for compatible in device.compatible() {
            if published.contains(&compatible) {
                continue;
            }

            match features.publish(compatible).await {
                Ok(()) => published.push(compatible),

                failure => {
                    log_fmt(format_args!(
                        "feature {compatible} not published: {failure:?}"
                    ));
                }
            }
        }
    }
}

fn open(shared: &Shared) -> Result<Port, SessionError> {
    let MintedSession { badge, port } = shared
        .service
        .create_session()
        .map_err(|_| SessionError::NoSpace)?;
    // Запись сессии создаётся до передачи порта драйверу, и первый кадр сессии её находит.
    shared
        .sessions
        .borrow_mut()
        .push(SessionRecord::new(badge.get()));
    Ok(port)
}
