//! 32-битные регистры окон устройства по ключу окна и их доступ через отображённые окна.

#![cfg_attr(not(test), no_std)]

mod registers;

pub use registers::{MappedWindows, Registers};
