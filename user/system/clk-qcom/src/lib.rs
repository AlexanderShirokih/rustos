//! Логика контроллера тактирования GCC Qualcomm: раскладка ветвей и генераторов тактов,
//! включение тактов узла по описанию платформы, частота такта по таблице его генератора и
//! таблицы тактов SoC.
#![no_std]

extern crate alloc;

mod gcc;
mod records;
mod sdm660;
mod table;

pub use gcc::{Gcc, Refusal, Registers};
pub use sdm660::SDM660;
pub use table::{Branch, Clock, Enable, Frequency, Generator};
