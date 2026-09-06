//! Собирает virtual timer ARM Generic Timer из `driver`/`service`/`state` и
//! экспортирует наружу только [`ArmGenericTimerDriver`] и `arm_generic_timer_probe`.
//! <https://developer.arm.com/documentation/100403/latest/>

mod driver;
mod service;
mod state;

pub use driver::{ArmGenericTimerDriver, arm_generic_timer_probe};
