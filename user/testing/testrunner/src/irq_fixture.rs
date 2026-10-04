#![no_std]
#![no_main]
#![allow(unsafe_code)]

use component_startup::{Requirements as _, component_manifest};
use kernel_objects::{MemoryRegion, Port};
use platform_abi::platform;
use runtime::{log_fmt, log_line, start_component};

mod fixture_start;

use fixture_start::{Failed, fixture_start};

const LOG_TAG: &str = "irq";

component_manifest! {
    struct IrqStartup {
        requirements: { platform: session(platform::Protocol) },
    }
}

fixture_start!(run);

fn run(parent: &Port) -> Result<(), Failed> {
    start_component(parent.as_raw(), LOG_TAG);
    let requirements = IrqStartup::request(parent.as_raw()).map_err(|_| Failed)?;

    let platform = requirements.platform.ok_or(Failed)?;
    let platform = platform.client();
    let device = ipc::wire::Str::new("/pl031@9010000").ok_or(Failed)?;
    let (Ok(Ok(_line)), Ok(Ok(window))) = (platform.irq(device, 0), platform.mmio(device, 0))
    else {
        log_line("resources refused");
        return Err(Failed);
    };
    let region = MemoryRegion::from(window.region)
        .inspect()
        .map_err(|_| Failed)?;
    log_fmt(format_args!("line and region {:#x}", region.base));
    Err(Failed)
}
