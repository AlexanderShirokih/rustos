//! Собирает GICv3 (Generic Interrupt Controller версии 3) из `controller`/`regs`/
//! `service` и экспортирует наружу только [`Gicv3`] и `gicv3_probe`.
//! <https://developer.arm.com/documentation/ihi0069/latest/>

mod controller;
mod driver;
mod regs;
mod service;

pub use driver::{Gicv3, gicv3_probe};
