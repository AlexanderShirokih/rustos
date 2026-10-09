//! Компонент `platform`: разбирает описание платформы, публикует каждую различную совместимость
//! доступных устройств как признак машины и выдаёт драйверам описание, линии прерываний и окна
//! регистров устройств по сессиям `Platform`.
//!
//! Компонент требует сессию `Hardware` с полномочиями на устройства и сессию `Features` для
//! публикации признаков. Выданные описание, линии и регионы принадлежат сессии, которая их взяла,
//! и отзываются с её концом либо с концом процесса `platform`.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::{boxed::Box, vec::Vec};
use core::{fmt, sync::atomic::AtomicU64};

use bootstrap_abi::{features, hardware};
use component_startup::{
    MintedSession, SessionError, StartupError, component_manifest, mint_session, serve_export,
};
use device_resolver::{DeviceTreePlatform, PLATFORM_BINDINGS};
use fdt::devicetree::DeviceTree;
use ipc::{Listener, RecvError, wire::MESSAGE_INLINE_MAX};
use kernel_objects::{IrqControl, MemoryRegion, Port, SessionTransport};
use platform_abi::{Description, platform};
use runtime::{FAILED_EXIT_CODE, Mutex, PortTransport, Timeout, exit, log_fmt, spawn_owned};

mod sessions;

use sessions::{HardwareDescription, Platform, SessionRecord};

component_manifest! {
    struct PlatformStartup {
        requirements: {
            hardware: session(hardware::Protocol),
            features: session(features::Protocol),
        },
        exports: [platform::Protocol],
    }
}

#[derive(Debug)]
enum Stopped {
    Startup(StartupError),
    Hardware,
    Description,
    ServicePort,
    ExportThread,
    ServicePortClosed,
}

struct Shared {
    sessions: Mutex<Vec<SessionRecord>>,
    service: Port,
    next_badge: AtomicU64,
}

impl fmt::Display for Stopped {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Startup(error) => error.fmt(formatter),
            Self::Hardware => formatter.write_str("hardware refused"),
            Self::Description => formatter.write_str("platform description malformed"),
            Self::ServicePort => formatter.write_str("service port refused"),
            Self::ExportThread => formatter.write_str("export thread refused"),
            Self::ServicePortClosed => formatter.write_str("service port closed"),
        }
    }
}

#[program_entry::component]
fn run(requirements: PlatformStartup) -> Result<(), Stopped> {
    let (service, platform) = start(requirements)?;
    serve(service, platform);
    Err(Stopped::ServicePortClosed)
}

fn start(requirements: PlatformStartup) -> Result<(&'static Port, Platform), Stopped> {
    let hardware = requirements
        .hardware
        .open()
        .map_err(Stopped::Startup)?
        .client();
    let lines = IrqControl::from(
        hardware
            .acquire_irq_control()
            .map_err(|_| Stopped::Hardware)?,
    );
    let fdt = hardware.acquire_boot_fdt().map_err(|_| Stopped::Hardware)?;
    let offset = usize::try_from(fdt.offset).map_err(|_| Stopped::Description)?;
    let region: &'static MemoryRegion = Box::leak(Box::new(MemoryRegion::from(fdt.region)));
    let mapping = Box::leak(Box::new(
        region
            .map_full_read_only()
            .map_err(|_| Stopped::Description)?,
    ));
    let tree = mapping
        .as_bytes()
        .get(offset..)
        .and_then(|bytes| DeviceTree::from_bytes(bytes).ok())
        .ok_or(Stopped::Description)?;
    let tree: &'static DeviceTree<'static> = Box::leak(Box::new(tree));

    let shared: &'static Shared = Box::leak(Box::new(Shared {
        sessions: Mutex::new(Vec::new()),
        service: Port::create().map_err(|_| Stopped::ServicePort)?,
        next_badge: AtomicU64::new(0),
    }));
    // Поток экспорта работает до конца процесса.
    let exporter = requirements.parent;
    let _ = spawn_owned(move || {
        let failure = serve_export::<platform::Protocol>(&exporter, |_| open(shared));
        log_fmt(format_args!("export stopped: {failure:?}"));
        exit(FAILED_EXIT_CODE)
    })
    .map_err(|_| Stopped::ExportThread)?;

    let description = Description::new(mapping.as_bytes(), u64::from(fdt.offset))
        .map_err(|_| Stopped::Description)?;
    let features = requirements.features.open().map_err(Stopped::Startup)?;
    publish(&features.client(), &description);

    Ok((
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

fn publish(features: &features::Client<SessionTransport>, description: &Description<'_>) {
    let mut published: Vec<&str> = Vec::new();
    for device in description.devices().filter_map(Result::ok) {
        for compatible in device.compatible() {
            if published.contains(&compatible) {
                continue;
            }

            match features.publish(compatible) {
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
