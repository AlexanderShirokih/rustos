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
use core::any::Any;

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
    /// Сильная ссылка на объект отзыва маппинга (`RevocationHook`,
    /// определённый выше по слою - в syscall). `memory` не знает его типа,
    /// поэтому держит как `Any`: единственная роль здесь - keep-alive.
    /// Дроп тега при `memory_free`/смерти AS роняет эту ссылку, и `Weak`
    /// в узле деривации повисает - повторный (ленивый) отзыв его пропускает.
    /// `None` для маппингов без публичной капы (анонимный `memory_allocate`).
    pub revocation: Option<Arc<dyn Any + Send + Sync>>,
}

pub type UserVmAllocator = RangeAllocator<MappingTag>;
