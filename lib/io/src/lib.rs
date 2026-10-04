//! Общие для ядра и userspace примитивы без стандартной библиотеки: запись байтов, доступ к MMIO,
//! вектор фиксированной ёмкости и SPSC-кольцо.

#![cfg_attr(not(test), no_std)]
#![cfg_attr(
    test,
    allow(clippy::cast_possible_truncation, clippy::std_instead_of_core)
)]

pub mod mmio;
pub mod spsc_ring;
pub mod vec;
pub mod writer;

pub use spsc_ring::{PopOutcome, PushOutcome, RingError, SpscRing};
pub use vec::Vec;
