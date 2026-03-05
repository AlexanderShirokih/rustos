//! Реализация GICv2 (Generic Interrupt Controller версии 2).
//! https://developer.arm.com/documentation/ihi0048/latest/

mod controller;
mod driver;
mod regs;
mod service;

pub use driver::{Gicv2, gicv2_probe};
