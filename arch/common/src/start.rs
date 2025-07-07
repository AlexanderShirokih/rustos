use crate::kernel::Kernel;
use core::hint;
use kernel_core::log::*;

pub fn main(kernel: &'static Kernel) -> ! {
    info("Kernel started!");

    loop {
        hint::spin_loop();
    }
}
