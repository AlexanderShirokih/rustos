//! Aarch64 backend для [`test_harness_qemu`]: QEMU `virt` semihosting + PL011.

#![no_std]
#![allow(unsafe_code)]

mod exit;
mod uart;

use test_harness_qemu::Backend;
pub use uart::Pl011Writer;

pub struct Aarch64Backend;

impl Backend for Aarch64Backend {
    fn exit(&self, code: u32) -> ! {
        exit::semihosting_exit(code)
    }
}

pub static BACKEND: Aarch64Backend = Aarch64Backend;
