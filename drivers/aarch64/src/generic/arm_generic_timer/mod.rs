//! Драйвер ARM Generic Timer.
//! https://developer.arm.com/documentation/100403/latest/

mod driver;
mod service;
mod state;

pub use driver::arm_generic_timer_probe;
pub use driver::ArmGenericTimerDriver;
