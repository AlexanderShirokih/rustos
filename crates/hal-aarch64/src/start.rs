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
mod sched;

#[cfg(feature = "kernel-tests")]
#[path = "kernel_tests/mod.rs"]
mod qemu_tests;

extern crate drivers_common_aarch64;

#[cfg(all(not(test), not(feature = "kernel-tests")))]
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

#[cfg(all(not(test), feature = "kernel-tests"))]
#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    use kernel_tests::Backend;

    klog::fatal!("[TEST-FAIL: panic] {}", info);
    test_harness_qemu_aarch64::BACKEND.exit(1)
}
