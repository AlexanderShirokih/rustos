//! Per-process аллокатор виртуального адресного пространства user-процесса.
//!
//! Тонкий alias над [`RangeAllocator<MemFlags>`]: tag - текущие флаги доступа
//! к региону. Освобождённые регионы возвращаются в free-list и сливаются с
//! соседними.

use crate::{MemFlags, range_allocator::RangeAllocator};

pub type UserVmAllocator = RangeAllocator<MemFlags>;
