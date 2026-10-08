#![no_std]
#![no_main]
#![allow(unsafe_code)]

runtime::process_heap!();

use core::panic::PanicInfo;

use component_startup::{Requirements as _, component_manifest};
use kernel_objects::Port;
use runtime::{Timeout, handle_close, log_line, process_exit, start_component, take_bootstrap};
use syscall::kind;

mod probe_protocol;

use probe_protocol::probe;

const LOG_TAG: &str = "closed";

const FAILED_EXIT_CODE: u64 = 1;

component_manifest! {
    struct ClosedStartup {
        requirements: { probe: session(probe::Protocol) },
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn _start(parent: u32) -> ! {
    // SAFETY: `parent` - значение `x0` точки входа, и точка входа исполняется один раз.
    unsafe { runtime::start(parent) };
    let Some(parent) = take_bootstrap() else {
        process_exit(FAILED_EXIT_CODE)
    };
    let parent = parent.assume_kind::<kind::Port>();
    start_component(parent.as_raw(), LOG_TAG);
    // Значение строится без открытия требования, и родитель не получает запроса.
    let _requirements = ClosedStartup::from_parent(parent.as_raw());
    log_line("started");
    if handle_close(parent) != 0 {
        process_exit(FAILED_EXIT_CODE)
    }

    let Ok(own) = Port::create() else {
        process_exit(FAILED_EXIT_CODE)
    };
    let _ = own.recv(Timeout::INFINITE);
    process_exit(FAILED_EXIT_CODE)
}

#[panic_handler]
fn panic(_info: &PanicInfo<'_>) -> ! {
    process_exit(FAILED_EXIT_CODE)
}
