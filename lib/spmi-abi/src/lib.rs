//! Контракты шины устройств PMIC за арбитром SPMI: [`spmi`] выдаёт сессию устройства по пути
//! его узла в описании, [`spmi_device`] читает и пишет регистры блоков устройства и выдаёт его
//! прерывания. [`controller`] находит в описании контроллер SPMI узла, по совместимости
//! которого потребитель выбирает шину.

#![cfg_attr(not(test), no_std)]

mod controller;
mod error;
mod protocol;

pub use controller::controller;
pub use error::{DeviceError, SpmiError};
pub use protocol::{spmi, spmi_device};
