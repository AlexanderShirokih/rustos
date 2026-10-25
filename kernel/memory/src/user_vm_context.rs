use alloc::{
    sync::{Arc, Weak},
    vec::Vec,
};
use core::num::NonZeroUsize;

use kbase::{LockCell, MutexCell};

use crate::{
    memory_mapper::{MemoryMapper, MemoryUnmappingError},
    user_vm_allocator::{MappingTag, UserVmAllocator},
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

    /// Снимает страницы и освобождает VA. Повторное освобождение отсутствующего
    /// диапазона проходит без последствий.
    pub fn unmap_range(&self, base: PageAlignedVirtualAddress, size: NonZeroUsize) {
        self.allocator.with_lock(|allocator| {
            if allocator.lookup(base, size).is_ok() {
                self.unmap_pages(base, size);
                let _ = allocator.free(base, size);
            }
        });
    }

    /// Отзывает доступ к страницам, сохраняя VA до явного освобождения.
    /// Проверка владельца и снятие страниц атомарны относительно освобождения
    /// и повторного выделения того же VA другим потоком процесса.
    pub fn revoke_range(
        &self,
        base: PageAlignedVirtualAddress,
        size: NonZeroUsize,
        belongs: impl FnOnce(&MappingTag) -> bool,
    ) {
        self.allocator.with_lock(|allocator| {
            let Ok(range) = allocator.lookup(base, size) else {
                return;
            };

            if !belongs(range.tag()) {
                return;
            }

            let revoked = MappingTag {
                flags: range.tag().flags,
                backing: None,
                grant: range.tag().grant,
            };

            self.unmap_pages(base, size);
            let _ = allocator.set_tag(base, size, revoked);
        });
    }

    fn unmap_pages(&self, base: PageAlignedVirtualAddress, size: NonZeroUsize) {
        match self.mapper.unmap(base, size.get()) {
            Ok(()) | Err(MemoryUnmappingError::NotMapped) => {}
            Err(error) => {
                debug_assert!(false, "unexpected unmap error: {error:?}");
            }
        }
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
