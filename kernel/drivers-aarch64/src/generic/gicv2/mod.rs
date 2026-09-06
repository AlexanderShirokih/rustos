//! Собирает GICv2 (Generic Interrupt Controller версии 2) из `controller`/`regs`/
//! `service` и экспортирует наружу только [`Gicv2`] и `gicv2_probe`.
//! <https://developer.arm.com/documentation/ihi0048/latest/>

mod controller;
mod driver;
mod regs;
mod service;

pub use driver::{Gicv2, gicv2_probe};
