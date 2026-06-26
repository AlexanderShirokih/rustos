//! Контракт и ошибки демо калькулятора.

#![no_std]

mod api;
mod error;

pub use api::*;
pub use error::{Error, Result};
