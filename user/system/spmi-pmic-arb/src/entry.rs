use alloc::{boxed::Box, vec::Vec};
use core::{panic::PanicInfo, sync::atomic::AtomicU64};

use component_startup::{
    Requirements as _, SessionError, component_manifest, mint_session, serve_export,
};
use kernel_objects::{IrqLine, MemoryRegion, Port, SessionTransport};
use platform_abi::{Description, platform};
use runtime::{
    Mutex, Priority, Timeout, log_fmt, log_line, process_exit, spawn_owned, start_component,
};
use spmi_abi::spmi;
use spmi_pmic_arb::{Arbiter, ArbiterNode, Refusal};
use syscall::{Handle, kind};

use crate::{
    bus::{Bus, Device, Sessions, serve},
    windows::Windows,
};

const LOG_TAG: &str = "spmi-pmic-arb";

const COMPATIBLE: &str = "qcom,spmi-pmic-arb";

const THREAD_STACK_SIZE: u64 = 0x10000;

runtime::process_heap!();

component_manifest! {
    struct SpmiStartup {
        requirements: { platform: session(platform::Protocol) },
        features: ["qcom,spmi-pmic-arb"],
        exports: [spmi::Protocol],
    }
}

#[allow(unsafe_code)]
#[unsafe(no_mangle)]
pub(crate) extern "C" fn _start(parent: Handle<kind::Port>) -> ! {
    start_component(parent, LOG_TAG);
    run(parent);
    process_exit(1)
}

fn run(parent: Handle<kind::Port>) {
    let requirements = SpmiStartup::from_parent(parent);
    // Сессия `Platform` держит окна и линию арбитра до конца процесса.
    let platform = match requirements.platform.open() {
        Ok(platform) => platform.client(),
        Err(error) => return log_fmt(format_args!("{error}")),
    };

    let Some((bus, line)) = take(&platform) else {
        return;
    };

    let Ok(service) = Port::create() else {
        return log_line("service port refused");
    };

    let bus: &'static Mutex<Bus> = Box::leak(Box::new(Mutex::new(bus)));
    let sessions: &'static Sessions = Box::leak(Box::new(Sessions {
        service,
        next_badge: AtomicU64::new(0),
    }));

    // Поток прерываний и поток экспорта работают до конца процесса.
    let interrupts = line.map(|line| {
        spawn_owned(
            move || {
                while line.wait(Timeout::INFINITE).is_ok() {
                    bus.lock().deliver();
                    if line.ack().is_err() {
                        break;
                    }
                }
                log_line("arbiter line lost");
                process_exit(1)
            },
            THREAD_STACK_SIZE,
            Priority::new(1),
        )
    });

    let exporter = requirements.parent;
    let export = spawn_owned(
        move || {
            let failure = serve_export::<spmi::Protocol>(&exporter, || {
                mint_session(&sessions.service, &sessions.next_badge)
                    .map(|minted| minted.port)
                    .map_err(|_| SessionError::NoSpace)
            });
            log_fmt(format_args!("export stopped: {failure:?}"));
            process_exit(1)
        },
        THREAD_STACK_SIZE,
        Priority::new(1),
    );

    if matches!(interrupts, Some(Err(_))) || export.is_err() {
        return log_line("service threads refused");
    }

    serve(bus, sessions);
}

fn take(platform: &platform::Client<SessionTransport>) -> Option<(Bus, Option<IrqLine>)> {
    let Ok(description) = platform.description() else {
        log_line("platform description refused");
        return None;
    };

    // Регион описания нужен только на время разбора.
    let region = MemoryRegion::from(description.region);
    let mapping = region.map_full_read_only().ok()?;
    let description = Description::new(mapping.as_bytes(), description.offset).ok()?;

    let Some(id) = description
        .devices()
        .filter_map(Result::ok)
        .find(|device| {
            device
                .compatible()
                .any(|compatible| compatible == COMPATIBLE)
        })
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
        .map(|_| platform.irq(id.as_wire(), 0))
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

#[panic_handler]
fn panic(info: &PanicInfo<'_>) -> ! {
    log_fmt(format_args!("panic: {info}"));
    process_exit(1)
}
