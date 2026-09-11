#![no_std]
#![no_main]
#![allow(unsafe_code)]

runtime::process_heap!();

use core::panic::PanicInfo;

use bootstrap_abi::bootstrap;
use kernel_objects::{Port, kind};
use log_abi::log;
use runtime::{PortTransport, thread_exit};
use syscall::Handle;

mod log_fixture_protocol;

use log_fixture_protocol::{
    EXIT_BOOT_FDT, EXIT_DEVICE_MEMORY, EXIT_IRQ_CONTROL, EXIT_LOG_REFUSED, EXIT_USERLAND_IMAGE,
    FIXTURE_LINE,
};

#[unsafe(no_mangle)]
pub extern "C" fn _start(log: Handle<kind::Port>) -> ! {
    let log = Port::from_handle(log);
    thread_exit(probe(&log))
}

fn probe(log: &Port) -> u64 {
    let bootstrap = bootstrap::Client::new(PortTransport::new(log.borrow()));
    if let Ok(Ok(_)) = bootstrap.acquire_irq_control() {
        return EXIT_IRQ_CONTROL;
    }
    if let Ok(Ok(_)) = bootstrap.acquire_userland_image() {
        return EXIT_USERLAND_IMAGE;
    }
    if let Ok(Ok(_)) = bootstrap.acquire_boot_fdt() {
        return EXIT_BOOT_FDT;
    }
    if let Ok(Ok(_)) = bootstrap.acquire_device_memory(0) {
        return EXIT_DEVICE_MEMORY;
    }

    let client = log::Client::new(PortTransport::new(log.borrow()));
    if client.write_str("probe", FIXTURE_LINE).is_err() {
        return EXIT_LOG_REFUSED;
    }
    0
}

#[panic_handler]
fn panic(_info: &PanicInfo<'_>) -> ! {
    loop {
        core::hint::spin_loop();
    }
}
