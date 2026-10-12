//! Контракт тактов узлов устройств: [`clocks`] включает такты узла у контроллера тактирования
//! и задаёт частоту такта.

#![cfg_attr(not(test), no_std)]

mod error;
mod protocol;

pub use error::ClocksError;
pub use protocol::clocks;
