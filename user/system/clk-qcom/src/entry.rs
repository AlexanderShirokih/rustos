use alloc::boxed::Box;
use core::fmt;

use clk_qcom::{Gcc, SDM660};
use clocks_abi::clocks;
use component_startup::{
    SessionPort, StartupError, component_manifest, serve_export, whole_export,
};
use kernel_objects::SessionTransport;
use platform_abi::{Description, DeviceClaims, MappedDescription, platform};
use runtime::{log_fmt, log_line, spawn_bound};

use crate::{
    sessions::{Controller, serve},
    window::Window,
};

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
fn run(requirements: ClocksStartup) -> Result<(), Stopped> {
    // Сессия `Platform` держит окно контроллера до конца процесса.
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
    let gcc = take(&platform, &description).ok_or(Stopped::Controller)?;
    let sessions = SessionPort::create().map_err(|_| Stopped::ServicePort)?;
    let sessions: &'static SessionPort = Box::leak(Box::new(sessions));

    // Поток экспорта работает до конца процесса.
    let exporter = requirements.parent;
    let export =
        spawn_bound(move || serve_export::<clocks::Protocol>(exporter, whole_export(sessions)));
    if export.is_err() {
        return Err(Stopped::ServiceThread);
    }

    let mut controller = Controller {
        description: &description,
        gcc,
        claims: DeviceClaims::new(),
    };
    serve(&mut controller, sessions);
    Err(Stopped::ServicePortClosed)
}

fn take(
    platform: &platform::Client<SessionTransport>,
    description: &Description<'_>,
) -> Option<Gcc<Window>> {
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
        .inspect_err(|error| log_fmt(format_args!("{} window refused: {error:?}", id.as_str())))
        .ok()?;

    Gcc::new(Window(window), SDM660, id.clone())
        .inspect_err(|error| log_fmt(format_args!("{} refused: {error:?}", id.as_str())))
        .ok()
}
