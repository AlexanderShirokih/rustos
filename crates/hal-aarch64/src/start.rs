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
mod exception;
mod memory;
mod sched;
mod system;

extern crate drivers_common_aarch64;

/// База higher-half (верхняя половина адресного пространства).
pub const HIGHER_HALF_BASE: usize = 0xFFFF_FF80_0000_0000;

#[cfg(all(not(test), not(feature = "qemu-tests")))]
#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    // SAFETY: `wfi` без операндов; вызов помещён в panic-handler - однопоточный
    // контекст, regular-инвариантов памяти не нарушает (nomem, nostack).
    unsafe {
        klog::fatal!("Kernel panic: {}", info);

        loop {
            core::arch::asm!("wfi", options(nomem, nostack));
        }
    }
}

#[cfg(all(not(test), feature = "qemu-tests"))]
#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    use qemu_test_harness::backend::Backend;

    klog::fatal!("[TEST-FAIL: panic] {}", info);
    qemu_test_harness_aarch64::BACKEND.exit(1)
}
