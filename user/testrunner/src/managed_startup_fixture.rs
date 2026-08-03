#![no_std]
#![no_main]
#![allow(unsafe_code)]

use core::panic::PanicInfo;

use runtime::{OwnedHandle, Signal, receive_startup, thread_exit};
use syscall::Handle;

mod managed_fixture_protocol;

use ipc::wire::Cap;
use managed_fixture_protocol::{ManagedFixtureService, dispatch_managed_fixture};

struct ManagedFixtureArgs {
    authority: Cap,
    exit_code: u32,
    keep_running: bool,
}

impl ManagedFixtureService for Option<ManagedFixtureArgs> {
    fn start(&mut self, authority: Cap, exit_code: u32, keep_running: bool) {
        *self = Some(ManagedFixtureArgs {
            authority,
            exit_code,
            keep_running,
        });
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn _start(startup: Handle) -> ! {
    let received = receive_startup(startup, dispatch_managed_fixture);
    let Ok(Some(ManagedFixtureArgs {
        authority,
        exit_code,
        keep_running,
    })) = received
    else {
        thread_exit(u64::from(u32::MAX));
    };

    let authority = Signal::from_handle(OwnedHandle::adopt(authority));
    if authority.notify_all().is_err() {
        thread_exit(u64::from(u32::MAX - 1));
    }
    if keep_running {
        loop {
            core::hint::spin_loop();
        }
    }
    thread_exit(u64::from(exit_code))
}

#[panic_handler]
fn panic(_info: &PanicInfo<'_>) -> ! {
    loop {
        core::hint::spin_loop();
    }
}
