//! Контракт и ошибки демо userspace-драйвера RTC.

#![no_std]

mod api;
mod error;

pub use api::*;
pub use error::{Error, Result};
