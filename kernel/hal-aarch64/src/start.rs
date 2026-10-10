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
#[cfg(not(test))]
mod panic;
mod power;
mod psci;
mod sched;
mod smp;

#[cfg(feature = "kernel-tests")]
#[path = "kernel_tests/mod.rs"]
mod qemu_tests;

extern crate drivers_common_aarch64;
