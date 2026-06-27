pub mod arm_generic_timer;
mod barrier;
mod dispatch;
pub mod gicv2;
pub mod gicv3;
pub mod pl011_uart;

pub(crate) use barrier::barrier_before_unmask;
pub(crate) use dispatch::{DispatchController, dispatch_interrupt};
