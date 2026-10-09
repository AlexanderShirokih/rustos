//! Источник ввода, который отказывает в подключении с `AttachError::Refused`.

#![no_std]
#![no_main]

extern crate alloc;

use component_startup::{Requirements as _, component_manifest};
use input_abi::input_source;
use syscall::{Handle, kind};

mod failed;
mod source_fixture;

use failed::Failed;
use source_fixture::{Attach, serve_source};

component_manifest! {
    struct RefusingInputSourceStartup {
        features: ["testrunner,featured"],
        exports: [input_source::Protocol],
    }
}

#[program_entry::component]
fn run(parent: Handle<kind::Port>) -> Result<(), Failed> {
    let parent = RefusingInputSourceStartup::from_parent(parent).parent;
    serve_source(parent, Attach::Refuse, None)
}
