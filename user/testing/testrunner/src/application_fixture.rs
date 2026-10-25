#![no_std]
#![no_main]

extern crate alloc;

use alloc::rc::Rc;

use application_abi::application;
use application_window::accept_window;
use component_startup::{AsyncStartup, component_manifest};
use executor::{Executor, PortChannel};
use syscall::{Handle, kind};

mod failed;

use failed::Failed;

component_manifest! {
    exports: [application::Protocol],
}

#[program_entry::component]
fn run(parent: Handle<kind::Port>) -> Result<(), Failed> {
    let executor = Rc::new(Executor::new().map_err(|_| Failed)?);
    executor.block_on(async {
        let parent = AsyncStartup::new(PortChannel::new(parent, executor.reactor()));
        let shown = accept_window(&executor, parent).await.map_err(|_| Failed)?;
        shown.session.serve().await;
        Ok(())
    })
}
