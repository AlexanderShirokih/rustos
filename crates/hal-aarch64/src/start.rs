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

#[cfg(feature = "kernel-tests")]
#[path = "kernel_tests/mod.rs"]
mod qemu_tests;

extern crate drivers_common_aarch64;

/// База higher-half (верхняя половина адресного пространства).
pub const HIGHER_HALF_BASE: usize = 0xFFFF_FF80_0000_0000;

/// База heap-арены, выше окна линейной PA->VA-карты
/// (`HIGHER_HALF_BASE + region.start`). Платформам с RAM > 480 GB
/// потребуется пересчёт.
pub const KHEAP_BASE: usize = 0xFFFF_FFF8_0000_0000;

/// VA-cap heap-арены. Физическая RAM приходит лениво при expand'е.
pub const KHEAP_MAX_SIZE: usize = 16 * 1024 * 1024 * 1024;

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
