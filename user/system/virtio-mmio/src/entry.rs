//! Точка входа процесса: требования, поток экспорта `VirtioMmio` и обслуживание шины.

use alloc::sync::Arc;
use core::fmt;

use component_startup::{
    SessionPort, StartupError, component_manifest, serve_export, whole_export,
};
use platform_abi::platform;
use runtime::spawn_bound;
use virtio_mmio_abi::virtio_mmio;

use crate::bus::Bus;

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
    let sessions = SessionPort::create().map_err(|_| Stopped::ServicePort)?;
    let sessions = Arc::new(sessions);

    let exporter = requirements.parent;
    let port = Arc::clone(&sessions);
    let _ =
        spawn_bound(move || serve_export::<virtio_mmio::Protocol>(exporter, whole_export(&port)))
            .map_err(|_| Stopped::ExportThread)?;

    bus.serve(&sessions);
    Err(Stopped::ServicePortClosed)
}
