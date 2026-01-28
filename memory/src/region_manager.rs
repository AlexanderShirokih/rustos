//! Менеджер регионов памяти
//!
//! Отвечает за знание о свободных и зарезервированных регионах памяти.
//! Предоставляет hint для HeapAllocator о том, где можно выделить память.

use crate::memory_range::MemoryRange;
use crate::virtual_address::VirtualAddress;
use collections::Vec;
use core::cmp::max;

/// Максимальное количество регионов
pub const MAX_REGIONS: usize = 24;

/// Менеджер регионов памяти.
/// Хранит информацию о свободных и зарезервированных регионах.
pub struct RegionManager {
    /// Свободные регионы
    regions: Vec<MemoryRange<VirtualAddress>, MAX_REGIONS>,
}

impl RegionManager {
    /// Создать пустой менеджер регионов
    pub const fn new(regions: Vec<MemoryRange<VirtualAddress>, MAX_REGIONS>) -> Self {
        Self { regions }
    }

    /// Найти свободное место размером >= size, начиная с cursor.
    /// Пропускает зарезервированные области.
    pub fn next_free(&self, cursor: VirtualAddress, size: usize) -> Option<VirtualAddress> {
        for free in self.regions.iter() {
            // Пропускаем регионы, которые полностью до cursor
            if free.end() <= cursor {
                continue;
            }

            let candidate = max(cursor, free.start());

            // Ищем позицию, не пересекающуюся с reserved
            loop {
                let candidate_end = candidate.offset(size);

                // Проверяем, что влезаем в free регион
                if candidate_end > free.end() {
                    break; // Не влезаем, переходим к следующему free региону
                }

                // Нашли подходящее место
                return Some(candidate);
            }
        }

        None
    }
}
