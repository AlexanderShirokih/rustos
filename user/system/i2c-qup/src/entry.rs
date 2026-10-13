use alloc::sync::Arc;
use core::fmt;

use clocks_abi::clocks;
use component_startup::{
    SessionPort, StartupError, component_manifest, serve_export, whole_export,
};
use i2c_abi::i2c;
use i2c_qup::Bus;
use pins_abi::pins;
use platform_abi::{MappedDescription, platform};
use runtime::spawn_bound;

use crate::{sessions::serve, supplies::Supplies};

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
    ServiceThread,
    ServicePortClosed,
}

impl fmt::Display for Stopped {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Startup(error) => error.fmt(formatter),
            Self::Description => formatter.write_str("platform description refused"),
            Self::ServicePort => formatter.write_str("service port refused"),
            Self::ServiceThread => formatter.write_str("service thread refused"),
            Self::ServicePortClosed => formatter.write_str("service port closed"),
        }
    }
}

#[program_entry::component]
fn run(requirements: I2cStartup) -> Result<(), Stopped> {
    // Сессия `Platform` держит окна и линии контроллеров до конца процесса.
    let platform = requirements
        .platform
        .open()
        .map_err(Stopped::Startup)?
        .client();
    let mapped = platform
        .description()
        .ok()
        .and_then(|region| MappedDescription::new(region).ok())
        .ok_or(Stopped::Description)?;
    let description = mapped.description().map_err(|_| Stopped::Description)?;

    let sessions = Arc::new(SessionPort::create().map_err(|_| Stopped::ServicePort)?);
    let exporter = requirements.parent;
    let port = Arc::clone(&sessions);
    spawn_bound(move || serve_export::<i2c::Protocol>(exporter, whole_export(&port)))
        .map_err(|_| Stopped::ServiceThread)?;

    let supplies = Supplies::new(&platform, requirements.pins, requirements.clocks);
    serve(&mut Bus::new(&description, supplies), &sessions);
    Err(Stopped::ServicePortClosed)
}
