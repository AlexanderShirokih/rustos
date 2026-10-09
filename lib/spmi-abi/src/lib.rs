//! Контракты шины устройств PMIC за арбитром SPMI: [`spmi`] выдаёт сессию устройства по
//! совместимости, [`spmi_device`] читает и пишет регистры блоков устройства и выдаёт его
//! прерывания.

#![cfg_attr(not(test), no_std)]

mod error;
mod protocol;

pub use error::{DeviceError, SpmiError};
pub use protocol::{COMPATIBLE_MAX, spmi, spmi_device};
