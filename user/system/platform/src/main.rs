//! Компонент `platform`: разбирает описание платформы, публикует каждую различную совместимость
//! доступных устройств как признак машины и выдаёт драйверам описание, линии прерываний и окна
//! регистров устройств по сессиям `Platform`.
//!
//! Компонент требует сессию `Hardware` с полномочиями на устройства и сессию `Features` для
//! публикации признаков. Выданные описание, линии и регионы принадлежат сессии, которая их взяла,
//! и отзываются с её концом либо с концом процесса `platform`.

#![no_std]
#![no_main]
#![allow(unsafe_code)]

runtime::process_heap!();

extern crate alloc;

use alloc::{boxed::Box, vec::Vec};
use core::{panic::PanicInfo, sync::atomic::AtomicU64};

use bootstrap_abi::{features, hardware};
use component_startup::{
    MintedSession, Requirements as _, SessionError, StartupError, component_manifest, mint_session,
    serve_export,
};
use device_resolver::{DeviceTreePlatform, PLATFORM_BINDINGS};
use fdt::devicetree::DeviceTree;
use ipc::{
    Listener, RecvError,
    wire::{MESSAGE_INLINE_MAX, Str},
};
use kernel_objects::{IrqControl, MemoryRegion, Port, SessionTransport};
use platform_abi::{Description, platform};
use runtime::{
    Mutex, PortTransport, Priority, Timeout, log_fmt, log_line, process_exit, spawn_owned,
    start_component,
};
use syscall::{Handle, kind};

mod sessions;

use sessions::{HardwareDescription, Platform, SessionRecord};

const LOG_TAG: &str = "platform";

const EXPORT_THREAD_STACK_SIZE: u64 = 0x10000;

const FAILED_EXIT_CODE: u64 = 1;

component_manifest! {
    struct PlatformStartup {
        requirements: {
            hardware: session(hardware::Protocol),
            features: session(features::Protocol),
        },
        exports: [platform::Protocol],
    }
}

struct Shared {
    sessions: Mutex<Vec<SessionRecord>>,
    service: Port,
    next_badge: AtomicU64,
}

#[unsafe(no_mangle)]
pub extern "C" fn _start(parent: Handle<kind::Port>) -> ! {
    start_component(parent, LOG_TAG);
    let Some((service, platform)) = start(parent) else {
        process_exit(FAILED_EXIT_CODE)
    };
    log_line("started");
    serve(service, platform);
    process_exit(FAILED_EXIT_CODE)
}

fn start(parent: Handle<kind::Port>) -> Option<(&'static Port, Platform)> {
    let requirements = PlatformStartup::from_parent(parent);
    let hardware = requirements
        .hardware
        .open()
        .inspect_err(log_error)
        .ok()?
        .client();
    let lines = IrqControl::from(hardware.acquire_irq_control().ok()?);
    let fdt = hardware.acquire_boot_fdt().ok()?;
    let offset = usize::try_from(fdt.offset).ok()?;
    let region: &'static MemoryRegion = Box::leak(Box::new(MemoryRegion::from(fdt.region)));
    let mapping = Box::leak(Box::new(region.map_full_read_only().ok()?));
    let tree = DeviceTree::from_bytes(mapping.as_bytes().get(offset..)?)
        .inspect_err(|_| log_line("platform description malformed"))
        .ok()?;
    let tree: &'static DeviceTree<'static> = Box::leak(Box::new(tree));

    let shared: &'static Shared = Box::leak(Box::new(Shared {
        sessions: Mutex::new(Vec::new()),
        service: Port::create().ok()?,
        next_badge: AtomicU64::new(0),
    }));
    // Поток экспорта работает до конца процесса.
    let exporter = requirements.parent;
    spawn_owned(
        move || {
            let failure = serve_export::<platform::Protocol>(&exporter, || open(shared));
            log_fmt(format_args!("export stopped: {failure:?}"));
            process_exit(FAILED_EXIT_CODE)
        },
        EXPORT_THREAD_STACK_SIZE,
        Priority::new(1),
    )
    .ok()?;

    let description = Description::new(mapping.as_bytes(), u64::from(fdt.offset)).ok()?;
    let features = requirements.features.open().inspect_err(log_error).ok()?;
    publish(&features.client(), &description);

    Some((
        &shared.service,
        Platform::new(
            &shared.sessions,
            DeviceTreePlatform::new(tree, PLATFORM_BINDINGS),
            HardwareDescription {
                region,
                offset: u64::from(fdt.offset),
            },
            lines,
            hardware,
        ),
    ))
}

fn log_error(error: &StartupError) {
    log_fmt(format_args!("{error}"));
}

fn publish(features: &features::Client<SessionTransport>, description: &Description<'_>) {
    let mut published: Vec<&str> = Vec::new();
    for device in description.devices().filter_map(Result::ok) {
        for compatible in device.compatible() {
            if published.contains(&compatible) {
                continue;
            }

            let Some(feature) = Str::new(compatible) else {
                log_fmt(format_args!("feature {compatible} not published: too long"));
                continue;
            };

            match features.publish(feature) {
                Ok(()) => published.push(compatible),

                failure => {
                    log_fmt(format_args!(
                        "feature {compatible} not published: {failure:?}"
                    ));
                }
            }
        }
    }
}

fn open(shared: &Shared) -> Result<Port, SessionError> {
    let MintedSession { badge, port } =
        mint_session(&shared.service, &shared.next_badge).map_err(|_| SessionError::NoSpace)?;
    // Запись сессии создаётся до передачи порта драйверу, и первый кадр сессии её находит.
    shared.sessions.lock().push(SessionRecord::new(badge.get()));
    Ok(port)
}

fn serve(service: &Port, mut platform: Platform) {
    let transport = PortTransport::new(service.borrow());
    let mut bytes = [0u8; MESSAGE_INLINE_MAX];
    loop {
        match transport.recv(&mut bytes, Timeout::INFINITE) {
            Ok(frame) => {
                let _ = platform::dispatch(&mut platform, frame);
            }
            Err(RecvError::SenderGone(badge)) => platform.close(badge),
            Err(RecvError::Discarded | RecvError::Timeout) => {}
            Err(RecvError::Closed) => break,
        }
    }
}

#[panic_handler]
fn panic(info: &PanicInfo<'_>) -> ! {
    log_fmt(format_args!("panic: {info}"));
    process_exit(FAILED_EXIT_CODE)
}
