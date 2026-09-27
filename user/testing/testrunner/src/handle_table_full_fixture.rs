#![no_std]
#![no_main]
#![allow(unsafe_code)]

runtime::process_heap!();

use core::panic::PanicInfo;

use kernel_objects::{Port, Signal, kind};
use runtime::{Timeout, handle_close, thread_exit};
use syscall::Handle;

const NO_PARKING_EXIT_CODE: u64 = 1;
const EMPTY_TABLE_EXIT_CODE: u64 = 2;
const RECEIVED_EXIT_CODE: u64 = 3;

#[unsafe(no_mangle)]
pub extern "C" fn _start(server: Handle<kind::Port>) -> ! {
    let server = Port::from_handle(server);
    let Ok(parking) = Port::create() else {
        thread_exit(NO_PARKING_EXIT_CODE);
    };

    let Some(spare) = fill_handle_table() else {
        thread_exit(EMPTY_TABLE_EXIT_CODE);
    };
    let _ = handle_close(spare);

    if server.recv(Timeout::INFINITE).is_ok() {
        thread_exit(RECEIVED_EXIT_CODE);
    }

    let _ = parking.recv(Timeout::INFINITE);
    thread_exit(NO_PARKING_EXIT_CODE)
}

fn fill_handle_table() -> Option<Handle<kind::Signal>> {
    let mut last = None;
    while let Ok(signal) = Signal::create() {
        last = Some(signal.into_raw());
    }
    last
}

#[panic_handler]
fn panic(_info: &PanicInfo<'_>) -> ! {
    loop {
        core::hint::spin_loop();
    }
}
