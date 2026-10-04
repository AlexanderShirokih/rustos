//! Структуры данных и примитивы блокировки, которыми пользуется только ядро.

#![no_std]
#![cfg_attr(
    test,
    allow(clippy::cast_possible_truncation, clippy::std_instead_of_core)
)]

pub mod bit_set;
pub mod interval_set;
pub mod lock_cell;
pub mod static_string;

pub use bit_set::{BitSet, bits_to_words};
pub use lock_cell::*;
pub use static_string::StaticString;
