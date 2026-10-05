//! Точка входа процесса: требования, поток экспорта `VirtioMmio` и обслуживание шины.

use alloc::boxed::Box;
use core::{panic::PanicInfo, sync::atomic::AtomicU64};

use component_startup::{
    Requirements as _, SessionError, component_manifest, mint_session, serve_export,
};
use kernel_objects::Port;
use platform_abi::platform;
use runtime::{Priority, log_fmt, log_line, process_exit, spawn_owned, start_component};
use syscall::{Handle, kind};
use virtio_mmio_abi::virtio_mmio;

use crate::bus::{Bus, Sessions};

const LOG_TAG: &str = "virtio-mmio";

const EXPORT_THREAD_STACK_SIZE: u64 = 0x10000;

runtime::process_heap!();

component_manifest! {
    struct VirtioMmioStartup {
        requirements: { platform: session(platform::Protocol) },
        features: ["virtio,mmio"],
        exports: [virtio_mmio::Protocol],
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
    let requirements = match VirtioMmioStartup::request(parent) {
        Ok(requirements) => requirements,
        Err(error) => return log_fmt(format_args!("{error}")),
    };
    // Сессия `Platform` держит окна и линии транспортов до конца процесса.
    let Some(platform) = requirements.platform.map(ipc::wire::Session::client) else {
        return log_line("platform session not granted");
    };
    let Some(mut bus) = Bus::take(&platform) else {
        return log_line("platform description refused");
    };
    let Ok(service) = Port::create() else {
        return log_line("service port refused");
    };
    let sessions: &'static Sessions = Box::leak(Box::new(Sessions {
        service,
        next_badge: AtomicU64::new(0),
    }));

    // Поток экспорта работает до конца процесса.
    let exporter = requirements.parent;
    let export_thread = spawn_owned(
        move || {
            let failure = serve_export::<virtio_mmio::Protocol>(&exporter, || {
                mint_session(&sessions.service, &sessions.next_badge)
                    .map(|minted| minted.port)
                    .map_err(|_| SessionError::NoSpace)
            });
            log_fmt(format_args!("export stopped: {failure:?}"));
            process_exit(1)
        },
        EXPORT_THREAD_STACK_SIZE,
        Priority::new(1),
    );
    if export_thread.is_err() {
        return log_line("export thread refused");
    }

    log_line("started");
    bus.serve(sessions);
}

#[panic_handler]
fn panic(info: &PanicInfo<'_>) -> ! {
    log_fmt(format_args!("panic: {info}"));
    process_exit(1)
}
