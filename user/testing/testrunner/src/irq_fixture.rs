#![no_std]
#![no_main]

use component_startup::{Requirements as _, component_manifest};
use kernel_objects::MemoryRegion;
use platform_abi::platform;
use runtime::{log_fmt, log_line};
use syscall::{Handle, kind};

mod failed;

use failed::Failed;

component_manifest! {
    struct IrqStartup {
        requirements: { platform: session(platform::Protocol) },
    }
}

#[program_entry::component]
fn run(parent: Handle<kind::Port>) -> Result<(), Failed> {
    let platform = IrqStartup::from_parent(parent)
        .platform
        .open()
        .map_err(|_| Failed)?
        .client();
    let device = "/pl031@9010000";
    let (Ok(_line), Ok(window)) = (platform.irq(device, 0), platform.mmio(device, 0)) else {
        log_line("resources refused");
        return Err(Failed);
    };
    let region = MemoryRegion::from(window.region)
        .inspect()
        .map_err(|_| Failed)?;
    log_fmt(format_args!("line and region {:#x}", region.base));
    Err(Failed)
}
