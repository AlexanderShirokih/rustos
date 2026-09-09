#![no_std]
#![no_main]
#![allow(unsafe_code)]

runtime::process_heap!();

extern crate alloc;

use core::panic::PanicInfo;

use rootkeeper::{STOP_TIMEOUT, Stopped, relay, supervise};
use runtime::thread_exit;
use syscall::{Handle, kind};

/// `root` приходит в x0 как HandleId bootstrap Port, переданного ядром.
#[unsafe(no_mangle)]
pub extern "C" fn _start(root: Handle<kind::Port>) -> ! {
    thread_exit(supervise(root).into())
}

#[panic_handler]
fn panic(_info: &PanicInfo<'_>) -> ! {
    // Если упал главный поток, relay не должен удерживать процесс живым.
    relay::shutdown(STOP_TIMEOUT);
    thread_exit(Stopped::Subsystem.into())
}
