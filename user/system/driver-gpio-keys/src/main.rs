//! Кнопки на линиях GPIO узла `gpio-keys`. Предоставляет протокол `InputSource`: объявляет
//! устройство ввода с клавишей на каждого доступного потомка узла и передаёт нажатия по
//! прерываниям линий с программным антидребезгом.

#![no_std]
#![no_main]

extern crate alloc;

mod driver;

use component_startup::component_manifest;
use input_abi::input_source;
use pins_abi::pins;
use platform_abi::platform;

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
fn run(requirements: GpioKeysStartup) -> Result<(), Stopped> {
    driver::run(
        requirements.pins,
        &requirements.platform,
        requirements.parent,
    )
}
