#![no_std]
#![no_main]
#![allow(unsafe_code)]

runtime::process_heap!();

use core::panic::PanicInfo;

use component_startup::{Requirements as _, component_manifest};
use kernel_objects::MemoryRegion;
use platform_abi::platform;
use runtime::{log_fmt, log_line, process_exit, start_component};
use syscall::{Handle, kind};

const LOG_TAG: &str = "irq";

const RESTART_EXIT_CODE: u64 = 1;

component_manifest! {
    struct IrqStartup {
        requirements: { platform: session(platform::Protocol) },
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn _start(parent: Handle<kind::Port>) -> ! {
    start_component(parent, LOG_TAG);
    let Ok(requirements) = IrqStartup::request(parent) else {
        process_exit(RESTART_EXIT_CODE)
    };

    let Some(platform) = requirements.platform else {
        process_exit(RESTART_EXIT_CODE)
    };
    let platform = platform.client();
    let Some(compatible) = ipc::wire::Str::new("arm,pl031") else {
        process_exit(RESTART_EXIT_CODE)
    };
    let (Ok(Ok(_line)), Ok(Ok(window))) = (
        platform.irq(compatible, 0, 0),
        platform.mmio(compatible, 0, 0),
    ) else {
        log_line("resources refused");
        process_exit(RESTART_EXIT_CODE)
    };
    let Ok(region) = MemoryRegion::from(window.region).inspect() else {
        process_exit(RESTART_EXIT_CODE)
    };
    log_fmt(format_args!("line and region {:#x}", region.base));
    process_exit(RESTART_EXIT_CODE)
}

#[panic_handler]
fn panic(_info: &PanicInfo<'_>) -> ! {
    process_exit(RESTART_EXIT_CODE)
}
