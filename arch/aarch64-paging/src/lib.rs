//! Управление таблицами страниц AArch64 (ARMv8-A).
//!
//! Реализует 4-уровневую схему трансляции адресов с гранулярностью 4 КБ.
//! Поддерживает блочные маппинги на уровнях L1 (1 ГБ) и L2 (2 МБ).

#![no_std]

extern crate alloc;

#[cfg(test)]
extern crate std;

pub mod entry;
pub mod level;
pub mod mapper;
pub mod mem_flags;
pub mod page_table;
pub mod preset;
pub mod table_alloc;
pub mod table_flags;
pub mod virtual_address;
