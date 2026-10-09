#![no_std]
#![no_main]

use kernel_objects::Port;
use runtime::{ExitCode, Timeout, spawn_owned};
use syscall::{Handle, kind};

const HELPER_SURVIVED_EXIT_CODE: u32 = 8;

#[program_entry::component]
fn run(_parent: Handle<kind::Port>) -> ExitCode {
    if let Ok(own) = Port::create()
        && spawn_owned(helper).is_ok()
    {
        let _ = own.recv(Timeout::INFINITE);
    }
    ExitCode::from(HELPER_SURVIVED_EXIT_CODE)
}

#[expect(
    clippy::panic,
    reason = "the fixture checks a panic of a helper thread"
)]
fn helper() -> u32 {
    panic!("helper thread panics")
}
