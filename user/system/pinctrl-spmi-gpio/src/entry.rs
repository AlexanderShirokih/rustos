use alloc::rc::Rc;
use core::{cell::RefCell, fmt};

use component_startup::{
    AsyncStartup, Requirements as _, SessionError, SessionPort, StartupError, component_manifest,
    serve_component,
};
use executor::{Executor, PortChannel};
use pins_abi::pins;
use platform_abi::{MappedDescription, platform};
use spmi_abi::spmi;
use syscall::{Handle, kind};

use crate::sessions::Driver;

component_manifest! {
    struct PinsStartup {
        requirements: {
            platform: session(platform::Protocol),
            spmi: all(spmi::Protocol),
        },
        features: ["qcom,spmi-gpio"],
        exports: [pins::Protocol => [{ controller = "qcom,spmi-gpio" }]],
    }
}

#[derive(Debug)]
enum Stopped {
    Startup(StartupError),
    ServicePort,
    Executor,
    ServicePortClosed,
}

impl fmt::Display for Stopped {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Startup(error) => error.fmt(formatter),
            Self::ServicePort => formatter.write_str("service port refused"),
            Self::Executor => formatter.write_str("executor refused"),
            Self::ServicePortClosed => formatter.write_str("service port closed"),
        }
    }
}

#[program_entry::component]
fn run(parent: Handle<kind::Port>) -> Result<(), Stopped> {
    let requirements = PinsStartup::from_parent(parent);
    let platform = requirements.platform.open().map_err(Stopped::Startup)?;

    let executor = Rc::new(Executor::new().map_err(|_| Stopped::Executor)?);
    let reactor = executor.reactor();

    let platform = PortChannel::client(platform, reactor.clone());

    let sessions = Rc::new(SessionPort::create().map_err(|_| Stopped::ServicePort)?);
    let driver = Rc::new(RefCell::new(None::<Rc<Driver>>));

    let dispatch = driver.clone();
    let bound = driver.clone();
    let closed = driver.clone();
    let port = sessions.clone();
    let mut providers = Some(requirements.spmi);

    let served = serve_component::<pins::Protocol, _>(
        &executor,
        AsyncStartup::new(PortChannel::new(parent, reactor.clone())),
        sessions,
        move |frame| {
            let driver = dispatch.borrow().clone();
            let driver = driver.ok_or(ipc::wire::IpcError::PeerClosed)?;
            Driver::serve(&driver, frame)
        },
        async move |_| {
            if bound.borrow().is_none() {
                let region = platform
                    .description()
                    .await
                    .map_err(|_| SessionError::Refused)?;

                let description =
                    MappedDescription::new(region).map_err(|_| SessionError::Refused)?;
                description
                    .description()
                    .map_err(|_| SessionError::Refused)?;
                let spmi = providers.take().ok_or(SessionError::Unavailable)?;
                *bound.borrow_mut() = Some(Rc::new(Driver::new(
                    description,
                    spmi,
                    port.clone(),
                    reactor.clone(),
                )));
            }

            port.open_session()
        },
        move |badge| {
            if let Some(driver) = closed.borrow().as_ref() {
                driver.release(badge);
            }
        },
    );

    let _ = executor.block_on(served);
    Err(Stopped::ServicePortClosed)
}
