#![no_std]
#![no_main]
#![allow(unsafe_code)]

runtime::process_heap!();

use core::panic::PanicInfo;

use component_startup::{LOG_REQUIREMENT, component_startup::Client, session};
use kernel_objects::PortTransport;
use log_abi::log;
use runtime::{install_log, log_fmt, process_exit};
use syscall::{Handle, Rights, kind};

const LOG_TAG: &str = "duplicate";

const RESTART_EXIT_CODE: u64 = 1;

#[unsafe(no_mangle)]
pub extern "C" fn _start(parent: Handle<kind::Port>) -> ! {
    let requests = Client::new(PortTransport::new(parent));
    let Ok(Ok(log)) = session::<log::Protocol>(&requests, LOG_REQUIREMENT) else {
        process_exit(RESTART_EXIT_CODE)
    };
    let duplicate = log.handle().duplicate(Rights::WRITE, None).map(drop);
    install_log(log.into_handle().into_issued(), LOG_TAG);
    log_fmt(format_args!("{duplicate:?}"));
    process_exit(RESTART_EXIT_CODE)
}

#[panic_handler]
fn panic(_info: &PanicInfo<'_>) -> ! {
    process_exit(RESTART_EXIT_CODE)
}
