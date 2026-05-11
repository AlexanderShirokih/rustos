//! Aarch64 backend для [`kernel_tests`]: QEMU `virt` semihosting + PL011.

#![no_std]
#![allow(unsafe_code)]

mod exit;
pub mod payload;
mod uart;

use kernel_tests::Backend;
pub use uart::Pl011Writer;

pub struct Aarch64Backend;

impl Backend for Aarch64Backend {
    fn exit(&self, code: u32) -> ! {
        exit::semihosting_exit(code)
    }
}

pub static BACKEND: Aarch64Backend = Aarch64Backend;
