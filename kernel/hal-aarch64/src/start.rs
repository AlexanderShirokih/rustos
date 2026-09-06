//! Корень крейта ядра AArch64.
//!
//! Точка входа: `_start` в `boot::boot_early`.
//! Pre-MMU фаза: `boot::boot_early::boot_main`.
//! Post-MMU фаза: `boot::boot_primary::primary_main`.

#![no_std]
#![no_main]
#![allow(unsafe_code)]
#![allow(
    clippy::cast_possible_truncation,
    clippy::indexing_slicing,
    clippy::multiple_unsafe_ops_per_block,
    clippy::panic,
    clippy::expect_used
)]

extern crate alloc;

#[cfg(test)]
extern crate std;

mod boot;
mod consts;
mod cpu;
mod exception;
mod memory;
#[cfg(feature = "power-semihosting")]
mod power;
mod sched;
mod smp;

#[cfg(feature = "kernel-tests")]
#[path = "kernel_tests/mod.rs"]
mod qemu_tests;

extern crate drivers_common_aarch64;

static PANICKING: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

#[cfg(not(test))]
#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    use core::sync::atomic::Ordering;

    use collections::{CriticalSection as _, DaifCriticalSection};

    // IRQ маскируются на весь путь паники, поэтому печать причины не прерывается,
    // а обработчик не вытесняется до выключения машины.
    let _irq_mask = DaifCriticalSection::enter();

    // Паника на пути печати причины гасит машину молча, потому что стек ядра защитной
    // страницы не имеет, и рекурсивная печать затирает соседнюю память.
    if PANICKING.swap(true, Ordering::Relaxed) {
        kernelspace::power::system_off(1);
    }

    // Причина печатается в обход локов пути записи, потому что их держатель - контекст,
    // прерванный паникой, и своих локов он уже не освобождает.
    klog::panic_logf(format_args!("Kernel panic: {info}"));
    kernelspace::power::system_off(1)
}
