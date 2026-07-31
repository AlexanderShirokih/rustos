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
mod cpu;
mod exception;
mod memory;
#[cfg(feature = "power-semihosting")]
mod power;
mod sched;

#[cfg(feature = "kernel-tests")]
#[path = "kernel_tests/mod.rs"]
mod qemu_tests;

extern crate drivers_common_aarch64;

/// Сток логов, который панике нужно перевести в неаллоцирующий режим.
static LOG_SINK: spin::Once<&'static io::buffered_writer::BufferedWriter> = spin::Once::new();

/// Отдаёт панике сток логов. Вызывается один раз, сразу после `set_stdout`.
pub(crate) fn set_log_sink(sink: &'static io::buffered_writer::BufferedWriter) {
    let _ = LOG_SINK.call_once(|| sink);
}

#[cfg(not(test))]
#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    // Паника достижима из аллокатора, поэтому накопление лога в куче
    // отключается до первой записи.
    if let Some(sink) = LOG_SINK.get() {
        sink.stop_buffering();
    }

    klog::fatal!("Kernel panic: {}", info);
    kernelspace::power::system_off(1)
}
