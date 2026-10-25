use component_startup::{Requirements as _, component_manifest};
use i2c_abi::i2c;
use input_abi::input_source;
use pins_abi::pins;
use platform_abi::platform;
use syscall::{Handle, kind};

use crate::driver::Stopped;

component_manifest! {
    struct TouchscreenStartup {
        requirements: {
            i2c: all(i2c::Protocol),
            pins: all(pins::Protocol),
            platform: session(platform::Protocol),
        },
        features: ["novatek,nt36672a-ts"],
        exports: [input_source::Protocol],
    }
}

#[program_entry::component]
fn run(parent: Handle<kind::Port>) -> Result<(), Stopped> {
    let requirements = TouchscreenStartup::from_parent(parent);
    crate::driver::run(
        requirements.i2c,
        requirements.pins,
        &requirements.platform,
        parent,
    )
}
