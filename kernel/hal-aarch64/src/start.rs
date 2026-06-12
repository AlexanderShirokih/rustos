//! Корень крейта ядра AArch64.
//!
//! Точка входа: `_start` в `boot::boot_early`.
//! Pre-MMU фаза: `boot::boot_early::boot_main`.
//! Post-MMU фаза: `boot::boot_primary::primary_main`.

#![no_std]
#![no_main]
#![allow(unsafe_code)]

extern crate alloc;

#[cfg(test)]
extern crate std;

mod boot;
mod consts;
mod exception;
mod memory;
#[cfg(feature = "power-semihosting")]
mod power;
mod sched;

#[cfg(feature = "kernel-tests")]
#[path = "kernel_tests/mod.rs"]
mod qemu_tests;

extern crate drivers_common_aarch64;

#[cfg(not(test))]
#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    klog::fatal!("Kernel panic: {}", info);
    kernelspace::power::system_off(1)
}
