//! Контракт `Platform`, по которому драйвер получает линии прерываний и окна регистров
//! устройств.
//!
//! Клиента и сервис даёт модуль [`platform`]. Запрос называет совместимость и номер устройства.
//! Выданные линия и регион отзываются с концом сессии.

#![cfg_attr(not(test), no_std)]

mod error;
mod protocol;
mod window;

pub use error::PlatformError;
pub use protocol::platform;
pub use window::MmioWindow;
