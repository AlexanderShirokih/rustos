#![no_std]
#![no_main]
#![allow(unsafe_code)]
#![allow(clippy::expect_used, clippy::indexing_slicing)]

runtime::process_heap!();

use core::panic::PanicInfo;

use runtime::{Timeout, handle_close, port_send, thread_exit};
use syscall::{Handle, SyscallError, kind};

mod ipc_raw;

const RECEIVER_WAIT: Timeout = Timeout::from_secs(5);
const FORWARDED_EXIT_CODE: u64 = 0;
const HANDLE_LOST_EXIT_CODE: u64 = 0x100;

#[unsafe(no_mangle)]
pub extern "C" fn _start(start: Handle<kind::Port>) -> ! {
    ipc_raw::write_frame(&[], &[start.raw()]);
    let sent = port_send(start, RECEIVER_WAIT);
    if handle_close(start) != 0 {
        thread_exit(HANDLE_LOST_EXIT_CODE);
    }
    match SyscallError::from_return(sent) {
        Some(error) => thread_exit(u64::from(error as u32)),
        None => thread_exit(FORWARDED_EXIT_CODE),
    }
}

#[panic_handler]
fn panic(_info: &PanicInfo<'_>) -> ! {
    loop {
        core::hint::spin_loop();
    }
}
