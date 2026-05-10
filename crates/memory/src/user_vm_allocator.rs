//! Per-process аллокатор виртуального адресного пространства user-процесса.
//!
//! Tag хранит текущие флаги доступа маппинга, `Arc<MemoryRegion>` -
//! владельца PA - и `grant`: потолок access-битов, разрешённый caller'у при
//! `MemoryMap`. `MemoryRemap` ограничен этим потолком, иначе процесс с
//! handle'ом на R-only мог бы апгрейднуть mapping до RW через remap, в
//! обход capability на handle (на этапе remap handle уже не проверяется).
//!
//! Освобождённый регион возвращается в free-list и сливается с соседними;
//! `Arc` дропается -> если ссылок больше нет, регион возвращает свои фреймы
//! аллокатору.

use alloc::sync::Arc;

use crate::{
    MemFlags,
    range_allocator::RangeAllocator,
    region::{AccessMask, MemoryRegion},
};

#[derive(Clone)]
pub struct MappingTag {
    pub flags: MemFlags,
    pub region: Arc<MemoryRegion>,
    pub grant: AccessMask,
}

pub type UserVmAllocator = RangeAllocator<MappingTag>;
