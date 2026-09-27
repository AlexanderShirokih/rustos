#![no_std]
#![no_main]
#![allow(unsafe_code)]

runtime::process_heap!();

use core::panic::PanicInfo;

use component_startup::{LOG_REQUIREMENT, component_startup::Client, session};
use kernel_objects::{Error, PortTransport};
use log_abi::log;
use runtime::{install_log, log_fmt, process_exit};
use syscall::{Handle, Rights, SyscallError, kind};

const LOG_TAG: &str = "duplicate";

const FAILED_EXIT_CODE: u64 = 1;
const REFUSED_EXIT_CODE: u64 = 3;
const OTHER_EXIT_CODE: u64 = 4;

#[unsafe(no_mangle)]
pub extern "C" fn _start(parent: Handle<kind::Port>) -> ! {
    let requests = Client::new(PortTransport::new(parent));
    let Ok(Ok(log)) = session::<log::Protocol>(&requests, LOG_REQUIREMENT) else {
        process_exit(FAILED_EXIT_CODE)
    };
    let duplicate = log.handle().duplicate(Rights::WRITE, None).map(drop);
    install_log(log.into_handle().into_raw(), LOG_TAG);
    log_fmt(format_args!("{duplicate:?}"));
    match duplicate {
        Err(Error::Syscall(SyscallError::AccessDenied)) => process_exit(REFUSED_EXIT_CODE),
        _ => process_exit(OTHER_EXIT_CODE),
    }
}

#[panic_handler]
fn panic(_info: &PanicInfo<'_>) -> ! {
    process_exit(FAILED_EXIT_CODE)
}
