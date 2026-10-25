use alloc::rc::Rc;
use core::{cell::RefCell, fmt};

use clk_qcom::{Gcc, SDM660};
use clocks_abi::clocks;
use component_startup::{
    AsyncStartup, Requirements as _, SessionError, SessionPort, StartupError, component_manifest,
    serve_component,
};
use executor::{Executor, PortChannel};
use kernel_objects::{MmioMapping, SessionTransport};
use platform_abi::{Description, DeviceClaims, MappedDescription, platform};
use runtime::{log_fmt, log_line};
use syscall::{Handle, kind};

use crate::sessions::Controller;

const COMPATIBLE: &str = "qcom,gcc-sdm660";
const WINDOW_RESOURCE: u16 = 0;

component_manifest! {
    struct ClocksStartup {
        requirements: { platform: session(platform::Protocol) },
        features: ["qcom,gcc-sdm660"],
        exports: [clocks::Protocol => [{ controller = "qcom,gcc-sdm660" }]],
    }
}

#[derive(Debug)]
enum Stopped {
    Startup(StartupError),
    Description,
    Controller,
    ServicePort,
    Executor,
    ServicePortClosed,
}

impl fmt::Display for Stopped {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Startup(error) => error.fmt(formatter),
            Self::Description => formatter.write_str("platform description refused"),
            Self::Controller => formatter.write_str("controller refused"),
            Self::ServicePort => formatter.write_str("service port refused"),
            Self::Executor => formatter.write_str("executor refused"),
            Self::ServicePortClosed => formatter.write_str("service port closed"),
        }
    }
}

#[program_entry::component]
fn run(parent: Handle<kind::Port>) -> Result<(), Stopped> {
    let requirements = ClocksStartup::from_parent(parent);
    let platform = requirements.platform.open().map_err(Stopped::Startup)?;

    let executor = Rc::new(Executor::new().map_err(|_| Stopped::Executor)?);
    let reactor = executor.reactor();

    let platform = PortChannel::client(platform, reactor.clone());

    let sessions = Rc::new(SessionPort::create().map_err(|_| Stopped::ServicePort)?);
    let controller = Rc::new(RefCell::new(None::<Rc<Controller>>));

    let dispatch = controller.clone();
    let bound = controller.clone();
    let closed = controller.clone();
    let port = sessions.clone();

    let served = serve_component::<clocks::Protocol, _>(
        &executor,
        AsyncStartup::new(PortChannel::new(parent, reactor)),
        sessions,
        move |frame| {
            let controller = dispatch.borrow().clone();
            let controller = controller.ok_or(ipc::wire::IpcError::PeerClosed)?;
            clocks::serve(&controller, frame)
        },
        async move |_| {
            if bound.borrow().is_none() {
                let controller = bind(&platform).await.map_err(|error| {
                    log_fmt(format_args!("{COMPATIBLE} not bound: {error}"));
                    SessionError::Refused
                })?;

                *bound.borrow_mut() = Some(Rc::new(controller));
            }

            port.open_session()
        },
        move |badge| {
            if let Some(controller) = closed.borrow().as_ref() {
                controller.claims.borrow_mut().release(badge);
            }
        },
    );

    let _ = executor.block_on(served);
    Err(Stopped::ServicePortClosed)
}

async fn bind(
    platform: &platform::Client<PortChannel<SessionTransport>>,
) -> Result<Controller, Stopped> {
    let region = platform
        .description()
        .await
        .map_err(|_| Stopped::Description)?;

    let mapped = MappedDescription::new(region).map_err(|_| Stopped::Description)?;
    let description = mapped.description().map_err(|_| Stopped::Description)?;
    let gcc = take(platform, &description)
        .await
        .ok_or(Stopped::Controller)?;

    Ok(Controller {
        description: mapped,
        gcc,
        claims: RefCell::new(DeviceClaims::new()),
    })
}

async fn take(
    platform: &platform::Client<PortChannel<SessionTransport>>,
    description: &Description<'_>,
) -> Option<Gcc<MmioMapping>> {
    let Some(id) = description
        .compatible_devices(COMPATIBLE)
        .next()
        .map(|device| device.id().clone())
    else {
        log_line("no controller described");
        return None;
    };

    let window = platform
        .map_mmio(&id, WINDOW_RESOURCE)
        .await
        .inspect_err(|error| log_fmt(format_args!("{} window refused: {error:?}", id.as_str())))
        .ok()?;

    Gcc::new(window, SDM660, id.clone())
        .inspect_err(|error| log_fmt(format_args!("{} refused: {error:?}", id.as_str())))
        .ok()
}
