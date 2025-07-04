#![no_std]
#![no_main]

mod kernel;

use crate::kernel::Kernel;
use crate::kernel::memory;
use crate::kernel::util::log;
use crate::kernel::util::log::{OutputStreamLogger};
use core::hint;
use spin::once::Once;

static EARLY_LOGGER: Once<OutputStreamLogger> = Once::new();

pub fn main(kernel: &'static Kernel) -> ! {
    // Install UART logger to control kernel loading
    let uart = kernel.device_registry().uart();
    let logger = EARLY_LOGGER.call_once(|| OutputStreamLogger::new(uart));

    log::set_logger(logger);

    log::info("Kernel awakened!");

    // Set up Memory Management
    memory::init(kernel);
    log::info("Memory initialized successfully");

    loop {
        hint::spin_loop();
    }
}
