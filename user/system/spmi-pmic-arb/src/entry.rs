use alloc::{boxed::Box, vec::Vec};
use core::{fmt, sync::atomic::AtomicU64};

use component_startup::{
    SessionError, StartupError, component_manifest, mint_session, serve_export,
};
use kernel_objects::{IrqLine, Port, SessionTransport};
use platform_abi::{MappedDescription, platform};
use runtime::{FAILED_EXIT_CODE, Mutex, Timeout, exit, log_fmt, log_line, spawn_owned};
use spmi_abi::spmi;
use spmi_pmic_arb::{Arbiter, ArbiterNode, Refusal};

use crate::{
    bus::{Bus, Device, Sessions, serve},
    windows::Windows,
};

const COMPATIBLE: &str = "qcom,spmi-pmic-arb";

component_manifest! {
    struct SpmiStartup {
        requirements: { platform: session(platform::Protocol) },
        features: ["qcom,spmi-pmic-arb"],
        exports: [spmi::Protocol],
    }
}

#[derive(Debug)]
enum Stopped {
    Startup(StartupError),
    Arbiter,
    ServicePort,
    ServiceThreads,
    ServicePortClosed,
}

impl fmt::Display for Stopped {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Startup(error) => error.fmt(formatter),
            Self::Arbiter => formatter.write_str("arbiter refused"),
            Self::ServicePort => formatter.write_str("service port refused"),
            Self::ServiceThreads => formatter.write_str("service threads refused"),
            Self::ServicePortClosed => formatter.write_str("service port closed"),
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
    let service = Port::create().map_err(|_| Stopped::ServicePort)?;

    let bus: &'static Mutex<Bus> = Box::leak(Box::new(Mutex::new(bus)));
    let sessions: &'static Sessions = Box::leak(Box::new(Sessions {
        service,
        next_badge: AtomicU64::new(0),
    }));

    // Поток прерываний и поток экспорта работают до конца процесса.
    let interrupts = line.map(|line| {
        spawn_owned(move || {
            while line.wait(Timeout::INFINITE).is_ok() {
                bus.lock().deliver();
                if line.ack().is_err() {
                    break;
                }
            }
            log_line("arbiter line lost");
            exit(FAILED_EXIT_CODE)
        })
    });

    let exporter = requirements.parent;
    let export = spawn_owned(move || {
        let failure = serve_export::<spmi::Protocol>(&exporter, |_| {
            mint_session(&sessions.service, &sessions.next_badge)
                .map(|minted| minted.port)
                .map_err(|_| SessionError::NoSpace)
        });
        log_fmt(format_args!("export stopped: {failure:?}"));
        exit(FAILED_EXIT_CODE)
    });

    if matches!(interrupts, Some(Err(_))) || export.is_err() {
        return Err(Stopped::ServiceThreads);
    }

    serve(bus, sessions);
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
        .map(|_| platform.irq(id.as_str(), 0))
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
    Some((Bus { arbiter, devices }, line))
}
