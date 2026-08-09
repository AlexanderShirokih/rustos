#![no_std]
#![no_main]
#![allow(unsafe_code)]

use core::panic::PanicInfo;

use runtime::{receive_startup, thread_exit};
use syscall::Handle;

mod startup_fixture_protocol;

use startup_fixture_protocol::{STARTUP_VALUE, startup_fixture};

#[unsafe(no_mangle)]
pub extern "C" fn _start(startup: Handle) -> ! {
    let received = receive_startup(startup, startup_fixture::dispatch);
    let passed = matches!(received, Ok(Some(STARTUP_VALUE)));
    thread_exit(u64::from(!passed))
}

#[panic_handler]
fn panic(_info: &PanicInfo<'_>) -> ! {
    loop {
        core::hint::spin_loop();
    }
}
