//! Источник ввода, который принимает заявки позже срока `STARTUP_REQUEST_TIMEOUT` от своего
//! запуска и объявляет устройство `SLOW_DEVICE`.

#![no_std]
#![no_main]

extern crate alloc;

use component_startup::{Requirements as _, component_manifest};
use input_abi::input_source;
use runtime::Timeout;
use syscall::{Handle, kind};

mod failed;
mod source_fixture;

use failed::Failed;
use source_fixture::{Attach, SLOW_DEVICE, serve_source};

const ACCEPT_DELAY: Timeout = Timeout::from_secs(7);

component_manifest! {
    struct SlowInputSourceStartup {
        features: ["testrunner,featured"],
        exports: [input_source::Protocol],
    }
}

#[program_entry::component]
fn run(parent: Handle<kind::Port>) -> Result<(), Failed> {
    let parent = SlowInputSourceStartup::from_parent(parent).parent;
    serve_source(parent, Attach::Declare(SLOW_DEVICE), Some(ACCEPT_DELAY))
}
