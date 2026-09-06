//! Структуры данных, живущие без кучи: вектор и строка фиксированной ёмкости, набор бит,
//! набор интервалов, SPSC-кольцо и ячейка под локом.
//!
//! [`Vec`] и [`StaticString`] держат элементы прямо в себе, ёмкость задана параметром типа.
//! [`BitSet`] хранит биты словами, [`interval_set::IntervalSet`] - непересекающиеся
//! интервалы.
//!
//! [`SpscRing`] переносит записи от одного писателя одному читателю через разделяемую память.
//! [`MutexCell`] закрывает данные локом. Вид критической секции этого лока задан параметром
//! типа. Под ядром на AArch64 секция гасит прерывания, на host [`NoCriticalSection`] их не
//! трогает.

#![no_std]
#![cfg_attr(
    test,
    allow(clippy::cast_possible_truncation, clippy::std_instead_of_core)
)]

pub mod bit_set;
pub mod interval_set;
pub mod lock_cell;
pub mod spsc_ring;
pub mod static_string;
pub mod vec;

pub use bit_set::{BitSet, bits_to_words};
pub use lock_cell::*;
pub use spsc_ring::{PopOutcome, PushOutcome, RingError, SpscRing};
pub use static_string::StaticString;
pub use vec::Vec;
