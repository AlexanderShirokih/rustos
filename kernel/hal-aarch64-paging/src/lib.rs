//! Отображает диапазоны в таблицах страниц AArch64 (ARMv8-A), снимает отображение и обходит
//! дерево таблиц.
//!
//! [`mapper::PageMapper`] работает по четырёхуровневой схеме трансляции с гранулярностью 4 КБ
//! и кладёт блочные отображения на уровнях L1 (1 ГБ) и L2 (2 МБ). Память под таблицы даёт
//! вызывающий реализацией `TableAlloc`. Он же держит корень дерева таблиц.
//!
//! Готовые наборы атрибутов страницы собраны в [`preset`]: текст ядра, его данные, его
//! константы, MMIO, куча.

#![no_std]
#![allow(unsafe_code)]
#![allow(
    clippy::cast_possible_truncation,
    clippy::indexing_slicing,
    clippy::panic
)]

extern crate alloc;

#[cfg(test)]
extern crate std;

pub mod entry;
pub mod level;
pub mod mapper;
pub mod mem_flags;
pub mod memory_attribute;
pub mod page_table;
pub mod preset;
pub mod table_alloc;
pub mod table_flags;
pub mod virtual_address;
