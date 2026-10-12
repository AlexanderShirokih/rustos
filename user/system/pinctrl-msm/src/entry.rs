use alloc::sync::Arc;
use core::fmt;

use component_startup::{
    SessionPort, StartupError, component_manifest, serve_export, whole_export,
};
use kernel_objects::SessionTransport;
use pinctrl_msm::{SDM660, Tlmm};
use pins_abi::{Claims, pins};
use platform_abi::{Description, MappedDescription, platform};
use runtime::{log_fmt, log_line, spawn_bound};

use crate::{
    sessions::{Controller, serve},
    windows::Windows,
};

const COMPATIBLE: &str = "qcom,sdm660-pinctrl";

component_manifest! {
    struct PinsStartup {
        requirements: { platform: session(platform::Protocol) },
        features: ["qcom,sdm660-pinctrl"],
        exports: [pins::Protocol => [{ controller = "qcom,sdm660-pinctrl" }]],
    }
}

#[derive(Debug)]
enum Stopped {
    Startup(StartupError),
    Description,
    Controller,
    ServicePort,
    ServiceThread,
    ServicePortClosed,
}

impl fmt::Display for Stopped {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Startup(error) => error.fmt(formatter),
            Self::Description => formatter.write_str("platform description refused"),
            Self::Controller => formatter.write_str("controller refused"),
            Self::ServicePort => formatter.write_str("service port refused"),
            Self::ServiceThread => formatter.write_str("service thread refused"),
            Self::ServicePortClosed => formatter.write_str("service port closed"),
        }
    }
}

#[program_entry::component]
fn run(requirements: PinsStartup) -> Result<(), Stopped> {
    // Сессия `Platform` держит окна контроллера до конца процесса.
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
    let tlmm = take(&platform, &description).ok_or(Stopped::Controller)?;
    let sessions = SessionPort::create().map_err(|_| Stopped::ServicePort)?;
    let sessions = Arc::new(sessions);

    let exporter = requirements.parent;
    let port = Arc::clone(&sessions);
    let export = spawn_bound(move || serve_export::<pins::Protocol>(exporter, whole_export(&port)));
    if export.is_err() {
        return Err(Stopped::ServiceThread);
    }

    let mut controller = Controller {
        description: &description,
        tlmm,
        claims: Claims::new(),
    };
    serve(&mut controller, &sessions);
    Err(Stopped::ServicePortClosed)
}

fn take(
    platform: &platform::Client<SessionTransport>,
    description: &Description<'_>,
) -> Option<Tlmm<Windows>> {
    let Some(id) = description
        .compatible_devices(COMPATIBLE)
        .next()
        .map(|device| device.id().clone())
    else {
        log_line("no controller described");
        return None;
    };

    let node = description.node(&id)?;
    let windows = Windows::take(platform, &id, &node)
        .inspect_err(|error| log_fmt(format_args!("{} windows refused: {error:?}", id.as_str())))
        .ok()?;

    Tlmm::new(windows, SDM660, description, id.clone())
        .inspect_err(|error| log_fmt(format_args!("{} refused: {error:?}", id.as_str())))
        .ok()
}
