#![no_std]
#![no_main]
#![allow(unsafe_code)]

runtime::process_heap!();

use core::panic::PanicInfo;

use kernel_objects::{Error, Signal};
use runtime::{PortTransport, Timeout, thread_exit};
use syscall::{Handle, SyscallError, kind};

mod answer_exhaustion_protocol;

use answer_exhaustion_protocol::{
    ANSWERED_EXIT_CODE, TABLE_NOT_FULL_EXIT_CODE, answer_exhaustion, exit_code_for,
};

const SERVER_WAIT: Timeout = Timeout::from_secs(5);

#[unsafe(no_mangle)]
pub extern "C" fn _start(start: Handle<kind::Port>) -> ! {
    if !fill_handle_table() {
        thread_exit(u64::from(TABLE_NOT_FULL_EXIT_CODE));
    }
    let client = answer_exhaustion::Client::new(PortTransport::new(start)).with_wait(SERVER_WAIT);
    let code = match client.request() {
        Ok(_) => ANSWERED_EXIT_CODE,
        Err(error) => exit_code_for(error),
    };
    thread_exit(u64::from(code))
}

fn fill_handle_table() -> bool {
    loop {
        match Signal::create() {
            Ok(signal) => {
                let _ = signal.into_raw();
            }
            Err(Error::Syscall(SyscallError::OutOfHandles)) => return true,
            Err(_) => return false,
        }
    }
}

#[panic_handler]
fn panic(_info: &PanicInfo<'_>) -> ! {
    loop {
        core::hint::spin_loop();
    }
}
