#![no_std]
#![no_main]
#![allow(clippy::expect_used, clippy::indexing_slicing)]

use kernel_objects::Port;
use runtime::{ExitCode, Timeout, port_send};
use syscall::SyscallError;

#[allow(unsafe_code)]
mod ipc_raw;

const RECEIVER_WAIT: Timeout = Timeout::from_secs(5);
const FAILED_EXIT_CODE: u32 = 0x100;

#[program_entry::process]
fn run(start: Port) -> ExitCode {
    ipc_raw::write_frame(&[], &[start.as_raw().raw()]);
    let sent = port_send(start.as_raw(), RECEIVER_WAIT);
    if start.close().is_err() {
        return ExitCode::from(FAILED_EXIT_CODE);
    }
    match SyscallError::from_return(sent) {
        Some(error) => ExitCode::from(error as u32),
        None => ExitCode::from(FAILED_EXIT_CODE),
    }
}
