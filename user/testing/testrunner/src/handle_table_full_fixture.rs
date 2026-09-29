#![no_std]
#![no_main]
#![allow(unsafe_code)]

runtime::process_heap!();

use core::panic::PanicInfo;

use kernel_objects::{Port, kind};
use runtime::{Timeout, handle_close, thread_exit};
use syscall::Handle;

mod handle_table_fill;

use handle_table_fill::fill_handle_table;

const FAILED_EXIT_CODE: u64 = 1;

#[unsafe(no_mangle)]
pub extern "C" fn _start(server: Handle<kind::Port>) -> ! {
    let server = Port::from_handle(server);
    let Ok(parking) = Port::create() else {
        thread_exit(FAILED_EXIT_CODE);
    };

    let Some(spare) = fill_handle_table() else {
        thread_exit(FAILED_EXIT_CODE);
    };
    let _ = handle_close(spare);

    if server.recv(Timeout::INFINITE).is_ok() {
        thread_exit(FAILED_EXIT_CODE);
    }

    let _ = parking.recv(Timeout::INFINITE);
    thread_exit(FAILED_EXIT_CODE)
}

#[panic_handler]
fn panic(_info: &PanicInfo<'_>) -> ! {
    loop {
        core::hint::spin_loop();
    }
}
