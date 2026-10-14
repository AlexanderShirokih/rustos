//! Контракты шины устройств PMIC за арбитром SPMI: [`spmi`] выдаёт сессию устройства по пути
//! его узла в описании, [`spmi_device`] читает и пишет регистры блоков устройства и выдаёт
//! сессии его прерываний, [`spmi_interrupt`] выдаёт `Signal` прерывания и подтверждает его
//! срабатывание. [`controller`] находит в описании контроллер SPMI узла, по совместимости
//! которого потребитель выбирает шину.

#![cfg_attr(not(test), no_std)]

mod controller;
mod error;
mod protocol;
mod trigger;

pub use controller::controller;
pub use error::{DeviceError, SpmiError};
pub use protocol::{spmi, spmi_device, spmi_interrupt};
pub use trigger::Trigger;
