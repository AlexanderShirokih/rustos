//! Корень крейта ядра AArch64.
//!
//! Точка входа: `_start` в `boot::boot_early`.
//! Pre-MMU фаза: `boot::boot_early::boot_main`.
//! Post-MMU фаза: `boot::boot_primary::primary_main`.

#![no_std]
#![no_main]

extern crate alloc;

#[cfg(test)]
extern crate std;

mod boot;
mod boot_header;
mod exception;
mod memory;
mod system;

extern crate drivers_common_aarch64;

/// База higher-half (верхняя половина адресного пространства).
pub const HIGHER_HALF_BASE: usize = 0xFFFF_FF80_0000_0000;

#[cfg(not(test))]
#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    unsafe {
        klog::fatal!("Kernel panic: {}", info);

        loop {
            core::arch::asm!("wfi", options(nomem, nostack));
        }
    }
}
