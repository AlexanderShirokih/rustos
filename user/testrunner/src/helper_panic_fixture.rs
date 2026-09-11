#![no_std]
#![no_main]
#![allow(unsafe_code)]

runtime::process_heap!();

use core::panic::PanicInfo;

use component_startup::component_manifest;
use kernel_objects::Port;
use runtime::{Priority, Timeout, process_exit, spawn_owned};
use syscall::{Handle, kind};

const HELPER_STACK: u64 = 0x4000;

const FAILED_EXIT_CODE: u64 = 1;
const PANIC_EXIT_CODE: u64 = 8;

component_manifest! {
    struct HelperPanicStartup {}
}

#[unsafe(no_mangle)]
pub extern "C" fn _start(parent: Handle<kind::Port>) -> ! {
    if HelperPanicStartup::start(parent).is_err() {
        process_exit(FAILED_EXIT_CODE)
    }
    let Ok(own) = Port::create() else {
        process_exit(FAILED_EXIT_CODE)
    };
    if spawn_owned(helper, HELPER_STACK, Priority::new(1)).is_err() {
        process_exit(FAILED_EXIT_CODE)
    }
    let _ = own.recv(Timeout::INFINITE);
    process_exit(FAILED_EXIT_CODE)
}

#[expect(
    clippy::panic,
    reason = "the fixture checks a panic of a helper thread"
)]
fn helper() -> u32 {
    panic!("helper thread panics")
}

#[panic_handler]
fn panic(_info: &PanicInfo<'_>) -> ! {
    process_exit(PANIC_EXIT_CODE)
}
