//! Драйвер тачскрина Novatek NT36672A на шине I2C. Экспортирует `InputSource` и передаёт
//! касания хабу ввода. Для работы требуется питание чипа, включённое загрузчиком.

#![no_std]
#![no_main]

extern crate alloc;

mod driver;

use component_startup::component_manifest;
use i2c_abi::i2c;
use input_abi::input_source;
use pins_abi::pins;
use platform_abi::platform;

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
fn run(requirements: TouchscreenStartup) -> Result<(), Stopped> {
    driver::run(
        requirements.i2c,
        requirements.pins,
        &requirements.platform,
        requirements.parent,
    )
}
