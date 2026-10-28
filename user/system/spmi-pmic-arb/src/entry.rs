use alloc::{rc::Rc, vec::Vec};
use core::{cell::RefCell, fmt};

use component_startup::{
    AsyncStartup, Requirements as _, SessionError, SessionPort, StartupError, component_manifest,
    serve_component,
};
use executor::{Executor, PortChannel, TaskHandle};
use kernel_objects::{IrqLine, SessionTransport};
use platform_abi::{MappedDescription, platform};
use runtime::{FAILED_EXIT_CODE, exit, log_fmt, log_line};
use spmi_abi::spmi;
use spmi_pmic_arb::{Arbiter, ArbiterNode, Refusal};
use syscall::{Handle, SIGNALED, WaitItem, kind};

use crate::{
    bus::{Bus, Device},
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
    ServicePort,
    Executor,
    ServicePortClosed,
}

struct Bound {
    bus: Rc<Bus>,
    _interrupts: Option<TaskHandle>,
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
    let requirements = SpmiStartup::from_parent(parent);
    let platform = requirements.platform.open().map_err(Stopped::Startup)?;

    let executor = Rc::new(Executor::new().map_err(|_| Stopped::Executor)?);

    let platform = PortChannel::client(platform);

    let sessions = Rc::new(SessionPort::create().map_err(|_| Stopped::ServicePort)?);
    let bound = Rc::new(RefCell::new(None::<Rc<Bound>>));

    let dispatch = bound.clone();
    let opened = bound.clone();
    let closed = bound.clone();
    let port = sessions.clone();
    let received = sessions.clone();
    let spawning = Rc::downgrade(&executor);

    let served = serve_component::<spmi::Protocol, core::future::Ready<_>>(
        &executor,
        AsyncStartup::new(PortChannel::new(parent)),
        sessions,
        move |frame| {
            let bound = dispatch.borrow().clone();
            let bound = bound.ok_or(ipc::wire::IpcError::PeerClosed)?;
            bound.bus.dispatch(&received, frame).map(|()| None)
        },
        async move |_| {
            if opened.borrow().is_none() {
                let (bus, line) = take(&platform).await.ok_or(SessionError::Refused)?;
                let bus = Rc::new(bus);
                let interrupts = if let Some(line) = line {
                    Some(
                        spawning
                            .upgrade()
                            .ok_or(SessionError::Unavailable)?
                            .spawn_now(deliver(line, bus.clone()))
                            .abort_on_drop(),
                    )
                } else {
                    None
                };
                *opened.borrow_mut() = Some(Rc::new(Bound {
                    bus,
                    _interrupts: interrupts,
                }));
            }

            port.open_session()
        },
        move |badge| {
            if let Some(bound) = closed.borrow().as_ref() {
                bound.bus.release(badge);
            }
        },
    );

    let _ = executor.block_on(served);
    Err(Stopped::ServicePortClosed)
}

async fn deliver(line: IrqLine, bus: Rc<Bus>) {
    loop {
        if executor::wait(WaitItem::new(line.as_raw(), SIGNALED))
            .await
            .is_err()
        {
            log_line("arbiter line lost");
            exit(FAILED_EXIT_CODE);
        }

        bus.deliver();
        if line.ack().is_err() {
            log_line("arbiter line lost");
            exit(FAILED_EXIT_CODE);
        }
    }
}

async fn take(
    platform: &platform::Client<PortChannel<SessionTransport>>,
) -> Option<(Bus, Option<IrqLine>)> {
    let Ok(description) = platform.description().await else {
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
        .await
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

    let line = if arbiter.is_some() {
        Some(
            platform
                .irq(id.clone(), 0)
                .await
                .inspect_err(|error| {
                    log_fmt(format_args!("{} line refused: {error:?}", id.as_str()));
                })
                .ok()?
                .into(),
        )
    } else {
        None
    };

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
            devices: RefCell::new(devices),
            departed_interrupts: RefCell::new(Vec::new()),
        },
        line,
    ))
}
