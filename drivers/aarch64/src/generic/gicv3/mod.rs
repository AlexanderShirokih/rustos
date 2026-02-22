//! Реализация GICv3 (Generic Interrupt Controller версии 3).
//! https://developer.arm.com/documentation/ihi0069/latest/

mod controller;
mod driver;
mod regs;
mod service;

pub use driver::Gicv3;
pub use driver::gicv3_probe;
