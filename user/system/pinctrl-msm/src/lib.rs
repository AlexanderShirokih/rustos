//! Логика контроллера выводов TLMM Qualcomm: раскладка регистров вывода, кодирование функции,
//! подтяжки и тока, применение состояния `default` узла по описанию платформы и таблицы
//! выводов SoC, линии выводов и их прерывания.
#![no_std]

extern crate alloc;

mod registers;
mod sdm660;
mod settings;
mod tlmm;

pub use registers::{PinGroup, Tile};
pub use sdm660::SDM660;
pub use tlmm::{Line, Plan, Refusal, Tlmm};
