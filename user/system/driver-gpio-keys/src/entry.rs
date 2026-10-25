use component_startup::{Requirements as _, component_manifest};
use input_abi::input_source;
use pins_abi::pins;
use platform_abi::platform;
use syscall::{Handle, kind};

use crate::driver::Stopped;

component_manifest! {
    struct GpioKeysStartup {
        requirements: {
            pins: all(pins::Protocol),
            platform: session(platform::Protocol),
        },
        features: ["gpio-keys"],
        exports: [input_source::Protocol],
    }
}

#[program_entry::component]
fn run(parent: Handle<kind::Port>) -> Result<(), Stopped> {
    let requirements = GpioKeysStartup::from_parent(parent);
    crate::driver::run(requirements.pins, &requirements.platform, parent)
}
