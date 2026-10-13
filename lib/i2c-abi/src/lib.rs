//! Контракты шины устройств I2C: [`i2c`] выдаёт сессию устройства за контроллером шины по пути
//! его узла в описании, [`i2c_device`] передаёт устройству байты и читает их от него.
//! Прочитанные байты приходят значением [`TransferBuf`].

#![cfg_attr(not(test), no_std)]

extern crate alloc;

mod buffer;
mod error;
mod protocol;

pub use buffer::TransferBuf;
pub use error::{I2cError, TransferError};
pub use protocol::{i2c, i2c_device};
