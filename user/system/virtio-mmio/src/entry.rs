//! Точка входа процесса: требования, поток экспорта `VirtioMmio` и обслуживание шины.

use alloc::boxed::Box;
use core::{fmt, sync::atomic::AtomicU64};

use component_startup::{
    SessionError, StartupError, component_manifest, mint_session, serve_export,
};
use kernel_objects::Port;
use platform_abi::platform;
use runtime::{FAILED_EXIT_CODE, exit, log_fmt, spawn_owned};
use virtio_mmio_abi::virtio_mmio;

use crate::bus::{Bus, Sessions};

component_manifest! {
    struct VirtioMmioStartup {
        requirements: { platform: session(platform::Protocol) },
        features: ["virtio,mmio"],
        exports: [virtio_mmio::Protocol],
    }
}

#[derive(Debug)]
enum Stopped {
    Startup(StartupError),
    Description,
    ServicePort,
    ExportThread,
    ServicePortClosed,
}

impl fmt::Display for Stopped {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Startup(error) => error.fmt(formatter),
            Self::Description => formatter.write_str("platform description refused"),
            Self::ServicePort => formatter.write_str("service port refused"),
            Self::ExportThread => formatter.write_str("export thread refused"),
            Self::ServicePortClosed => formatter.write_str("service port closed"),
        }
    }
}

#[program_entry::component]
fn run(requirements: VirtioMmioStartup) -> Result<(), Stopped> {
    // Сессия `Platform` держит окна и линии транспортов до конца процесса.
    let platform = requirements
        .platform
        .open()
        .map_err(Stopped::Startup)?
        .client();
    let mut bus = Bus::take(&platform).ok_or(Stopped::Description)?;
    let service = Port::create().map_err(|_| Stopped::ServicePort)?;
    let sessions: &'static Sessions = Box::leak(Box::new(Sessions {
        service,
        next_badge: AtomicU64::new(0),
    }));

    // Поток экспорта работает до конца процесса.
    let exporter = requirements.parent;
    let _ = spawn_owned(move || {
        let failure = serve_export::<virtio_mmio::Protocol>(&exporter, |_| {
            mint_session(&sessions.service, &sessions.next_badge)
                .map(|minted| minted.port)
                .map_err(|_| SessionError::NoSpace)
        });
        log_fmt(format_args!("export stopped: {failure:?}"));
        exit(FAILED_EXIT_CODE)
    })
    .map_err(|_| Stopped::ExportThread)?;

    bus.serve(sessions);
    Err(Stopped::ServicePortClosed)
}
