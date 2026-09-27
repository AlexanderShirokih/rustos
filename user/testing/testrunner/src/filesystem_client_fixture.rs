#![no_std]
#![no_main]
#![allow(unsafe_code)]

runtime::process_heap!();

use core::panic::PanicInfo;

use filesystem_abi::{AccessMode, OpenFlags, file_system};
use ipc::wire::Str;
use runtime::{PortTransport, thread_exit};
use syscall::{Handle, Timeout, kind};

const CALL_TIMEOUT: Timeout = Timeout::from_secs(2);

const BUFFER_SIZE: u32 = 64 * 1024;

const PATH: &str = "/fixture";

#[unsafe(no_mangle)]
pub extern "C" fn _start(service: Handle<kind::Port>) -> ! {
    thread_exit(u64::from(!populate_session(service)))
}

fn populate_session(service: Handle<kind::Port>) -> bool {
    let client = file_system::Client::new(PortTransport::new(service)).with_wait(CALL_TIMEOUT);

    let Ok(Ok(_buffer)) = client.acquire_buffer(BUFFER_SIZE) else {
        return false;
    };

    let Some(path) = Str::new(PATH) else {
        return false;
    };
    let opened = client.open(path, AccessMode::ReadWrite, OpenFlags::CREATE);
    matches!(opened, Ok(Ok(_)))
}

#[panic_handler]
fn panic(_info: &PanicInfo<'_>) -> ! {
    thread_exit(1)
}
