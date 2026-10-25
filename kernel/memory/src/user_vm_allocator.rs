//! Per-process аллокатор виртуального адресного пространства user-процесса.
//!
//! Tag действующего отображения хранит флаги доступа, `Arc<MemoryRegion>` -
//! владельца PA - и `grant`: потолок access-битов, разрешённый caller при
//! `MemoryMap`. `MemoryRemap` ограничен этим потолком, иначе процесс с
//! handle на R-only апгрейднет mapping до RW через remap, в
//! обход capability на handle (на этапе remap handle уже не проверяется).
//!
//! Освобождённый регион возвращается в free-list и сливается с соседними.
//! `Arc` дропается, и когда счётчик ссылок доходит до нуля, регион возвращает свои
//! фреймы аллокатору. Отзыв снимает страницы и освобождает регион, сохраняя
//! запись VA до явного освобождения клиентом или завершения процесса.

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
    /// Страницы отображения до явного освобождения либо отзыва. После отзыва `None`
    /// сохраняет занятый VA до `MemoryFree`, не удерживая физические страницы.
    pub backing: Option<MappingBacking>,
    pub grant: AccessMask,
}

/// Регион действующего отображения и его объект отзыва.
#[derive(Clone)]
pub struct MappingBacking {
    pub region: Arc<MemoryRegion>,
    /// Сильная ссылка на объект отзыва маппинга (`RevocationHook`,
    /// определённый выше по слою - в syscall). `memory` не проверяет его тип
    /// и держит как `Any`: единственная роль здесь - keep-alive.
    /// Отзыв, `memory_free` и завершение AS снимают эту ссылку. Слабая ссылка
    /// узла деривации после этого не позволяет повторно вызвать отзыв.
    /// `None` для маппингов без публичной капы (анонимный `memory_allocate`).
    pub revocation: Option<Arc<dyn Any + Send + Sync>>,
}

pub type UserVmAllocator = RangeAllocator<MappingTag>;
