use alloc::{
    sync::{Arc, Weak},
    vec::Vec,
};
use core::num::NonZeroUsize;

use kbase::{LockCell, MutexCell};

use crate::{
    memory_mapper::{MemoryMapper, MemoryUnmappingError},
    user_vm_allocator::UserVmAllocator,
    virtual_address::PageAlignedVirtualAddress,
};

/// Хэндл на per-process user-память, передаваемый syscall-handler-ам. Mapper и
/// аллокатор диапазонов общие через `Arc` с другими держателями контекста.
pub struct UserVmContext {
    mapper: Arc<dyn MemoryMapper + Send + Sync>,
    allocator: Arc<MutexCell<UserVmAllocator>>,
}

impl UserVmContext {
    pub fn new(
        mapper: Arc<dyn MemoryMapper + Send + Sync>,
        allocator: Arc<MutexCell<UserVmAllocator>>,
    ) -> Self {
        Self { mapper, allocator }
    }

    pub fn mapper(&self) -> &dyn MemoryMapper {
        &*self.mapper
    }

    /// Клонирует `Arc` на mapper. Вызывающий получает сильную ссылку, не привязанную к
    /// времени жизни `&self`. Единственный держатель такой ссылки - слой port-IPC: он
    /// хранит её в транспорте заблокированного потока (кросс-AS rendezvous).
    pub fn mapper_arc(&self) -> Arc<dyn MemoryMapper + Send + Sync> {
        self.mapper.clone()
    }

    pub fn allocator(&self) -> &Arc<MutexCell<UserVmAllocator>> {
        &self.allocator
    }

    /// Единый teardown маппинга. Снимает PTE и возвращает range в аллокатор.
    /// Зовётся из `memory_free` и из отзыва капы. Идемпотентен: `NotMapped`/
    /// `NotFound` трактуются как успех (двойной close / гонка close-vs-free).
    pub fn unmap_range(&self, base: PageAlignedVirtualAddress, size: NonZeroUsize) {
        match self.mapper.unmap(base, size.get()) {
            Ok(()) | Err(MemoryUnmappingError::NotMapped) => {}
            Err(e) => {
                // Выровненный range из аллокатора не бывает misaligned/block-mapped.
                // В release это no-op: range всё равно возвращаем в аллокатор -
                // держать его занятым (а с ним и `Arc` региона) бессмысленно, раз
                // PTE в желаемое состояние уже не вернуть.
                debug_assert!(false, "unmap_range: unexpected unmap error: {e:?}");
            }
        }
        self.allocator.with_lock(|alloc| {
            let _ = alloc.free(base, size);
        });
    }

    /// Снимает все зарегистрированные маппинги и освобождает их теги.
    pub fn drain(&self) {
        let ranges: Vec<_> = self.allocator.with_lock(|allocator| {
            allocator
                .allocated()
                .iter()
                .map(|range| (range.base(), range.pages()))
                .collect()
        });
        for (base, pages) in ranges {
            self.unmap_range(base, pages.saturating_mul(crate::PAGE_SIZE));
        }
    }

    /// Слабый хэндл, не удерживающий AS живым. Отзыв капы (revocation) использует именно
    /// такой хэндл, а сам отзыв хранится в аллокаторе через `MappingTag.revocation` -
    /// weak предотвращает цикл: аллокатор -> тег -> отзыв -> аллокатор.
    pub fn downgrade(&self) -> WeakUserVmContext {
        WeakUserVmContext {
            mapper: Arc::downgrade(&self.mapper),
            allocator: Arc::downgrade(&self.allocator),
        }
    }
}

/// Слабая версия [`UserVmContext`]: `upgrade` даёт `None`, когда AS уже
/// разрушено (процесс завершился) - тогда срывать нечего.
#[derive(Clone)]
pub struct WeakUserVmContext {
    mapper: Weak<dyn MemoryMapper + Send + Sync>,
    allocator: Weak<MutexCell<UserVmAllocator>>,
}

impl WeakUserVmContext {
    pub fn upgrade(&self) -> Option<UserVmContext> {
        Some(UserVmContext::new(
            self.mapper.upgrade()?,
            self.allocator.upgrade()?,
        ))
    }
}
