//! Разбирает записи device tree, принятые на AArch64: спецификатор прерывания GIC, affinity
//! процессора, MMIO-окна узла.
//!
//! [`adapt_to_fdt_tree`] переводит разобранное `fdt`-дерево в вид, который принимают
//! probe-функции `drivers-common`. [`gic_interrupt::parse_gic_interrupt`] достаёт из свойства
//! `interrupts` номер линии и её тип срабатывания.
//! [`device_windows::enumerate_device_windows`] перечисляет MMIO-окна узла.
//!
//! Крейт разбирает байты и не обращается к регистрам. Тесты крейта идут на host.

#![no_std]
#![allow(clippy::cast_possible_truncation)]

extern crate alloc;

pub mod affinity_frames;
mod commons;
pub mod device_windows;
pub mod fdt_adapter;
pub mod gic_interrupt;
pub mod ipi_affinity;
pub mod tree_ext;
pub use commons::*;
pub use fdt_adapter::adapt_to_fdt_tree;
