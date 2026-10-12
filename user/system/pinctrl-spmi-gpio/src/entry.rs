use alloc::{collections::BTreeMap, sync::Arc, vec::Vec};
use core::fmt;

use component_startup::{
    SessionPort, StartupError, component_manifest, serve_export, whole_export,
};
use pins_abi::{Claims, pins};
use platform_abi::{MappedDescription, platform};
use runtime::{Mutex, spawn_bound};
use spmi_abi::spmi;

use crate::sessions::{Driver, serve};

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
fn run(requirements: PinsStartup) -> Result<(), Stopped> {
    // Сессия `Platform` держит регион описания до конца процесса.
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
    let sessions = SessionPort::create().map_err(|_| Stopped::ServicePort)?;
    let sessions = Arc::new(sessions);

    let exporter = requirements.parent;
    let port = Arc::clone(&sessions);
    let export = spawn_bound(move || serve_export::<pins::Protocol>(exporter, whole_export(&port)));
    if export.is_err() {
        return Err(Stopped::ServiceThread);
    }

    let driver = Mutex::new(Driver {
        description: &description,
        spmi: &requirements.spmi,
        gpios: Vec::new(),
        claims: Claims::new(),
        sessions: BTreeMap::new(),
    });
    serve(&driver, &sessions);
    Err(Stopped::ServicePortClosed)
}
