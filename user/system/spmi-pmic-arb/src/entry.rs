use alloc::{sync::Arc, vec::Vec};
use core::fmt;

use component_startup::{
    SessionPort, StartupError, component_manifest, serve_export, whole_export,
};
use kernel_objects::{IrqLine, SessionTransport};
use platform_abi::{MappedDescription, platform};
use runtime::{Mutex, Timeout, log_fmt, log_line, spawn_bound};
use spmi_abi::spmi;
use spmi_pmic_arb::{Arbiter, ArbiterNode, Refusal};

use crate::{
    bus::{Bus, Device, serve},
    windows::Windows,
};

const COMPATIBLE: &str = "qcom,spmi-pmic-arb";

component_manifest! {
    struct SpmiStartup {
        requirements: { platform: session(platform::Protocol) },
        features: ["qcom,spmi-pmic-arb"],
        exports: [spmi::Protocol => [{ controller = "qcom,spmi-pmic-arb" }]],
    }
}

#[derive(Debug)]
enum Stopped {
    Startup(StartupError),
    Arbiter,
    ServicePort,
    ServiceThreads,
    ServicePortClosed,
    ArbiterLine,
}

impl fmt::Display for Stopped {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Startup(error) => error.fmt(formatter),
            Self::Arbiter => formatter.write_str("arbiter refused"),
            Self::ServicePort => formatter.write_str("service port refused"),
            Self::ServiceThreads => formatter.write_str("service threads refused"),
            Self::ServicePortClosed => formatter.write_str("service port closed"),
            Self::ArbiterLine => formatter.write_str("arbiter line lost"),
        }
    }
}

#[program_entry::component]
fn run(requirements: SpmiStartup) -> Result<(), Stopped> {
    // Сессия `Platform` держит окна и линию арбитра до конца процесса.
    let platform = requirements
        .platform
        .open()
        .map_err(Stopped::Startup)?
        .client();
    let (bus, line) = take(&platform).ok_or(Stopped::Arbiter)?;
    let sessions = SessionPort::create().map_err(|_| Stopped::ServicePort)?;

    let bus = Arc::new(Mutex::new(bus));
    let sessions = Arc::new(sessions);

    let delivered = Arc::clone(&bus);
    let interrupts = line.map(|line| {
        spawn_bound(move || {
            while line.wait(Timeout::INFINITE).is_ok() {
                delivered.lock().deliver();
                if line.ack().is_err() {
                    break;
                }
            }
            Stopped::ArbiterLine
        })
    });

    let exporter = requirements.parent;
    let port = Arc::clone(&sessions);
    let export = spawn_bound(move || serve_export::<spmi::Protocol>(exporter, whole_export(&port)));

    if matches!(interrupts, Some(Err(_))) || export.is_err() {
        return Err(Stopped::ServiceThreads);
    }

    serve(&bus, &sessions);
    Err(Stopped::ServicePortClosed)
}

fn take(platform: &platform::Client<SessionTransport>) -> Option<(Bus, Option<IrqLine>)> {
    let Ok(description) = platform.description() else {
        log_line("platform description refused");
        return None;
    };

    let mapped = MappedDescription::new(description).ok()?;
    let description = mapped.description().ok()?;

    let Some(id) = description
        .compatible_devices(COMPATIBLE)
        .next()
        .map(|device| device.id().clone())
    else {
        log_line("no arbiter described");
        return None;
    };

    let node = ArbiterNode::parse(&description, &id)
        .inspect_err(|error| log_fmt(format_args!("{} refused: {error:?}", id.as_str())))
        .ok()?;
    let windows = Windows::take(platform, &id, &node)
        .inspect_err(|error| log_fmt(format_args!("{} windows refused: {error:?}", id.as_str())))
        .ok()?;

    let arbiter = match Arbiter::new(windows, node.ee()) {
        Ok(arbiter) => {
            log_fmt(format_args!(
                "{:?} arbiter, {} channels, ee {}",
                arbiter.version(),
                arbiter.channels(),
                node.ee()
            ));
            Some(arbiter)
        }
        Err(Refusal::Version(version)) => {
            log_fmt(format_args!("unsupported arbiter version {version:#010x}"));
            None
        }
        Err(Refusal::Window) => {
            log_line("arbiter windows too small");
            return None;
        }
    };

    let line = arbiter
        .as_ref()
        .map(|_| platform.irq(id.clone(), 0))
        .transpose()
        .inspect_err(|error| log_fmt(format_args!("{} line refused: {error:?}", id.as_str())))
        .ok()?
        .map(IrqLine::from);

    let mut devices = Vec::new();
    for found in node.peripherals(&description, &id) {
        match found {
            Ok(peripheral) if arbiter.is_some() => devices.push(Device {
                peripheral,
                holder: None,
            }),
            Ok(_) => {}
            Err(skipped) => log_fmt(format_args!(
                "{} skipped: {:?}",
                skipped.id.as_str(),
                skipped.reason
            )),
        }
    }

    log_fmt(format_args!("{} devices", devices.len()));
    Some((
        Bus {
            arbiter,
            devices,
            departed_interrupts: Vec::new(),
        },
        line,
    ))
}
