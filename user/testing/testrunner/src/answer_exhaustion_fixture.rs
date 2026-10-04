#![no_std]
#![no_main]
#![allow(unsafe_code)]

runtime::process_heap!();

use core::panic::PanicInfo;

use runtime::{PortTransport, Timeout, thread_exit};
use syscall::{Handle, kind};

mod answer_exhaustion_protocol;
mod handle_table_fill;

use answer_exhaustion_protocol::{
    ANSWERED_EXIT_CODE, TABLE_NOT_FULL_EXIT_CODE, answer_exhaustion, exit_code_for,
};
use handle_table_fill::fill_handle_table;

const SERVER_WAIT: Timeout = Timeout::from_secs(5);

#[unsafe(no_mangle)]
pub extern "C" fn _start(start: Handle<kind::Port>) -> ! {
    let Some(_spare) = fill_handle_table() else {
        thread_exit(u64::from(TABLE_NOT_FULL_EXIT_CODE));
    };
    let client = answer_exhaustion::Client::new(PortTransport::new(start)).with_wait(SERVER_WAIT);
    let code = match client.request() {
        Ok(_) => ANSWERED_EXIT_CODE,
        Err(error) => exit_code_for(error),
    };
    thread_exit(u64::from(code))
}

#[panic_handler]
fn panic(_info: &PanicInfo<'_>) -> ! {
    loop {
        core::hint::spin_loop();
    }
}
