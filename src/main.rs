#![no_std]
#![no_main]

mod kernel;

use crate::kernel::core::streams::OutputStreamExt;
use crate::kernel::dev::registry::DeviceRegistry;
use core::hint;
use core::panic::PanicInfo;

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    loop {}
}

pub fn main(registry: DeviceRegistry) -> ! {
    let hello = "Hello world using Device tree blob!";

    registry.uart().write_str(hello).unwrap();

    loop {
        hint::spin_loop();
    }
}
