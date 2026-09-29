#![no_std]
#![no_main]
#![allow(unsafe_code)]

runtime::process_heap!();

component_manifest! {
    exports: [hardware::Protocol, image::Protocol],
}

extern crate alloc;

use core::panic::PanicInfo;

use bootstrap_abi::{hardware, image};
use component_startup::component_manifest;
use rootkeeper::{Stopped, run};
use runtime::process_exit;
use syscall::{Handle, kind};

/// `root` приходит в x0 как HandleId bootstrap Port, переданного ядром.
#[unsafe(no_mangle)]
pub extern "C" fn _start(root: Handle<kind::Port>) -> ! {
    process_exit(run(root).into())
}

#[panic_handler]
fn panic(_info: &PanicInfo<'_>) -> ! {
    process_exit(Stopped::Subsystem.into())
}
