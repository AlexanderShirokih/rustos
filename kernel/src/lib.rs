//! Ядро операционной системы.
//!
//! Содержит основную логику инициализации, подсистему драйверов
//! и точку входа `kmain`.

#![no_std]
extern crate alloc;

// Реэкспорт foundation для совместимости
pub use foundation;

pub mod kmain;
