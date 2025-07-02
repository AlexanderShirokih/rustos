#![no_std]
#![no_main]
mod kernel;

use crate::kernel::core::streams::OutputStreamExt;
use crate::kernel::device::registry::DeviceRegistry;
use crate::kernel::panic::init_panic_handler;
use core::hint;

pub fn main(registry: DeviceRegistry) -> ! {
    init_panic_handler(&registry);

    registry.uart().write_str("Hello world").unwrap();

    loop {
        hint::spin_loop();
    }
}
