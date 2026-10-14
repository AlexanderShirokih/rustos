use alloc::sync::Arc;
use core::fmt;

use component_startup::{
    SessionPort, StartupError, component_manifest, serve_export, whole_export,
};
use kernel_objects::{IrqLine, SessionTransport};
use pinctrl_msm::{SDM660, Tlmm};
use pins_abi::{Claims, Lines, pins};
use platform_abi::{Description, MappedDescription, platform};
use runtime::{Mutex, Timeout, log_fmt, log_line, spawn_bound};

use crate::{
    hardware::Hardware,
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
    ServiceThreads,
    ServicePortClosed,
    SummaryLine,
}

impl fmt::Display for Stopped {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Startup(error) => error.fmt(formatter),
            Self::Description => formatter.write_str("platform description refused"),
            Self::Controller => formatter.write_str("controller refused"),
            Self::ServicePort => formatter.write_str("service port refused"),
            Self::ServiceThreads => formatter.write_str("service threads refused"),
            Self::ServicePortClosed => formatter.write_str("service port closed"),
            Self::SummaryLine => formatter.write_str("summary line lost"),
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
    let (tlmm, line) = take(&platform, &description).ok_or(Stopped::Controller)?;
    let sessions = SessionPort::create().map_err(|_| Stopped::ServicePort)?;

    let hardware = Arc::new(Mutex::new(Hardware::new(tlmm)));
    let sessions = Arc::new(sessions);

    let delivered = Arc::clone(&hardware);
    let interrupts = spawn_bound(move || deliver(&line, &delivered));

    let exporter = requirements.parent;
    let port = Arc::clone(&sessions);
    let export = spawn_bound(move || serve_export::<pins::Protocol>(exporter, whole_export(&port)));

    if interrupts.is_err() || export.is_err() {
        return Err(Stopped::ServiceThreads);
    }

    let mut controller = Controller {
        description: &description,
        hardware: &hardware,
        claims: Claims::new(),
        lines: Lines::new(),
    };
    serve(&mut controller, &sessions);
    Err(Stopped::ServicePortClosed)
}

fn deliver(line: &IrqLine, hardware: &Mutex<Hardware>) -> Stopped {
    let mut reported = false;
    while line.wait(Timeout::INFINITE).is_ok() {
        if !hardware.lock().deliver() && !reported {
            log_line("summary interrupt without a fired pin");
            reported = true;
        }
        if line.ack().is_err() {
            break;
        }
    }
    Stopped::SummaryLine
}

fn take(
    platform: &platform::Client<SessionTransport>,
    description: &Description<'_>,
) -> Option<(Tlmm<Windows>, IrqLine)> {
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

    let tlmm = Tlmm::new(windows, SDM660, description, id.clone())
        .inspect_err(|error| log_fmt(format_args!("{} refused: {error:?}", id.as_str())))
        .ok()?;
    let line = platform
        .irq(id.clone(), 0)
        .inspect_err(|error| log_fmt(format_args!("{} line refused: {error:?}", id.as_str())))
        .ok()?;

    Some((tlmm, IrqLine::from(line)))
}
