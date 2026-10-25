use alloc::rc::Rc;
use core::fmt;

use clocks_abi::clocks;
use component_startup::{
    AsyncStartup, Requirements as _, SessionPort, StartupError, component_manifest,
    serve_component, whole_export,
};
use executor::{Executor, PortChannel};
use i2c_abi::i2c;
use i2c_qup::Bus;
use pins_abi::pins;
use platform_abi::{MappedDescription, platform};
use syscall::{Handle, kind};

use crate::{sessions::Service, supplies::Supplies};

component_manifest! {
    struct I2cStartup {
        requirements: {
            clocks: all(clocks::Protocol),
            pins: all(pins::Protocol),
            platform: session(platform::Protocol),
        },
        features: ["qcom,i2c-qup-v2.2.1"],
        exports: [i2c::Protocol => [{ controller = "qcom,i2c-qup-v2.2.1" }]],
    }
}

#[derive(Debug)]
enum Stopped {
    Startup(StartupError),
    Description,
    ServicePort,
    ServicePortClosed,
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

#[program_entry::component]
fn run(parent: Handle<kind::Port>) -> Result<(), Stopped> {
    let requirements = I2cStartup::from_parent(parent);

    let executor = Rc::new(Executor::new().map_err(|_| Stopped::ServicePort)?);
    let reactor = executor.reactor();

    let platform = requirements.platform.open().map_err(Stopped::Startup)?;

    let platform = PortChannel::client(platform, reactor.clone());

    let description = platform
        .description()
        .wait()
        .map_err(|_| Stopped::Description)?;

    let mapped = MappedDescription::new(description).map_err(|_| Stopped::Description)?;

    let sessions = Rc::new(SessionPort::create().map_err(|_| Stopped::ServicePort)?);
    let supplies = Supplies::new(
        platform,
        requirements.pins,
        requirements.clocks,
        reactor.clone(),
    );
    let service = Rc::new(Service {
        bus: Bus::new(supplies),
        description: mapped,
        sessions: sessions.clone(),
    });

    let gone = service.clone();
    let exported = sessions.clone();

    let served = serve_component::<i2c::Protocol, _>(
        &executor,
        AsyncStartup::new(PortChannel::new(parent, reactor)),
        sessions,
        move |frame| Service::serve(&service, frame),
        whole_export(exported),
        move |badge| gone.bus.release(badge),
    );

    let _ = executor.block_on(served);
    Err(Stopped::ServicePortClosed)
}
