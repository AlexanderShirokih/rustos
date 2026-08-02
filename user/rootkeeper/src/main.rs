#![no_std]
#![no_main]
#![allow(unsafe_code)]

extern crate alloc;

use core::panic::PanicInfo;

use bootstrap::BootstrapClient;
use log_abi::LogClient;
use log_relay::LogRelayThread;
use runtime::{MemoryRegion, OwnedHandle, PortTransport, Timeout, UserMemFlags, thread_exit};
use syscall::Handle;
use userland_image::USERLAND_IMAGE_MAGIC;

mod relay;

const LOG_TAG: &str = "supervisor";

// Код выхода супервизора: им ядро гасит машину.
#[derive(Clone, Copy)]
enum Stopped {
    Startup = 1,
    BadImage = 2,
    Subsystem = 3,
}

impl From<Stopped> for u64 {
    fn from(stopped: Stopped) -> Self {
        stopped as Self
    }
}

/// `root` приходит в x0 как HandleId bootstrap Port, переданного ядром.
#[unsafe(no_mangle)]
pub extern "C" fn _start(root: Handle) -> ! {
    let kernel = OwnedHandle::from_handle(root);
    thread_exit(supervise(&kernel).into())
}

// Отказы relay идут в bootstrap-канал: по отказавшему пути диагностика не проходит.
fn supervise(kernel: &OwnedHandle) -> Stopped {
    let kernel_log = BootstrapClient::new(PortTransport::client(kernel.borrow()));

    let Ok(relay) = relay::start(kernel.as_raw()) else {
        let _ = kernel_log.log_str(LOG_TAG, "log relay not started");
        return Stopped::Startup;
    };

    match bring_up(&relay, &kernel_log) {
        Ok(()) => {
            let _ = relay.wait(Timeout::INFINITE);
            let _ = kernel_log.log_str(LOG_TAG, "log relay stopped");
            Stopped::Subsystem
        }
        // Код процесса берёт последний вышедший поток: цикл relay
        // останавливается до возврата супервизора.
        Err(stopped) => {
            let _ = relay.shutdown(Timeout::INFINITE);
            stopped
        }
    }
}

fn bring_up(
    relay: &LogRelayThread,
    kernel_log: &BootstrapClient<PortTransport>,
) -> Result<(), Stopped> {
    let Ok(session) = relay.session(relay::SUPERVISOR_BADGE) else {
        let _ = kernel_log.log_str(LOG_TAG, "supervisor log session not minted");
        return Err(Stopped::Startup);
    };
    let log = LogClient::new(PortTransport::client(&session));
    let _ = log.write_str(LOG_TAG, "started");

    if !image_carries_magic(kernel_log) {
        let _ = log.write_str(LOG_TAG, "userland image magic mismatch");
        return Err(Stopped::BadImage);
    }
    let _ = log.write_str(LOG_TAG, "userland image verified");
    Ok(())
}

// Read-only регион userland-образа по bootstrap-контракту, маппинг и сверка
// первых байт с USERLAND_IMAGE_MAGIC.
fn image_carries_magic(client: &BootstrapClient<PortTransport>) -> bool {
    let Ok(Ok(cap)) = client.acquire_userland_image() else {
        return false;
    };
    let region = MemoryRegion::from_handle(OwnedHandle::adopt(cap));

    let Ok(info) = region.inspect() else {
        return false;
    };
    let Ok(mapping) = region.map(info.size_bytes, UserMemFlags::ReadOnly) else {
        return false;
    };

    let mut magic = [0u8; USERLAND_IMAGE_MAGIC.len()];
    if mapping.copy_out(&mut magic).is_err() {
        return false;
    }
    magic == USERLAND_IMAGE_MAGIC
}

#[panic_handler]
fn panic(_info: &PanicInfo<'_>) -> ! {
    loop {
        core::hint::spin_loop();
    }
}
