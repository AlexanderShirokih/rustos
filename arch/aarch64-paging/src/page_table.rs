//! Таблица страниц AArch64.

use core::marker::PhantomData;

use crate::{
    entry::{Entry, Kind},
    level::Level,
};

/// Таблица страниц уровня `L`.
///
/// Содержит 512 записей (4 КБ).
pub struct PageTable<L: Level> {
    /// 512 записей по 8 байт.
    entries: [u64; 512],
    _p: PhantomData<L>,
}

impl<L: Level> PageTable<L> {
    pub const fn new() -> Self {
        Self {
            entries: [0; 512],
            _p: PhantomData,
        }
    }

    /// Записывает entry по индексу.
    pub fn set<K: Kind>(&mut self, idx: usize, e: Entry<L, K>) {
        self.entries[idx] = e.raw();
    }

    /// Возвращает сырое значение записи по индексу.
    pub fn get_raw(&self, idx: usize) -> u64 {
        self.entries[idx]
    }
}

impl<L: Level> Default for PageTable<L> {
    fn default() -> Self {
        Self::new()
    }
}
