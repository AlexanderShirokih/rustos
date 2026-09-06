use alloc::{
    sync::{Arc, Weak},
    vec::Vec,
};
use core::num::NonZeroUsize;

use collections::{LockCell, MutexCell};

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

#[cfg(test)]
mod tests {
    use alloc::{vec, vec::Vec};

    use super::*;
    use crate::{
        AccessMask, MemFlags, MemoryRegion,
        memory_mapper::{
            AddressSpaceHandle, AddressSpaceTag, MemoryMappingError, MemoryRemappingError,
        },
        physical_address::{PageAlignedAddress, PhysicalAddress},
        user_vm_allocator::MappingTag,
        virtual_address::VirtualAddress,
    };

    const PAGE: usize = crate::PAGE_SIZE.get();
    const ARENA: usize = 0x4000_0000;

    struct TrackingMapper {
        unmapped: MutexCell<Vec<(usize, usize)>>,
    }

    impl TrackingMapper {
        fn new() -> Self {
            Self {
                unmapped: MutexCell::new(Vec::new()),
            }
        }
    }

    impl MemoryMapper for TrackingMapper {
        fn map(
            &self,
            _v: PageAlignedVirtualAddress,
            _pc: usize,
            _i: &[u8],
            _f: MemFlags,
        ) -> Result<(), MemoryMappingError> {
            Ok(())
        }
        fn map_exact(
            &self,
            _v: PageAlignedVirtualAddress,
            _p: PageAlignedAddress,
            _s: usize,
            _f: MemFlags,
        ) -> Result<(), MemoryMappingError> {
            Ok(())
        }
        fn unmap(
            &self,
            v: PageAlignedVirtualAddress,
            s: usize,
        ) -> Result<(), MemoryUnmappingError> {
            let already = self
                .unmapped
                .with_lock(|u| u.iter().any(|&(b, _)| b == v.as_usize()));
            if already {
                return Err(MemoryUnmappingError::NotMapped);
            }
            self.unmapped.with_lock(|u| u.push((v.as_usize(), s)));
            Ok(())
        }
        fn remap(
            &self,
            _v: PageAlignedVirtualAddress,
            _s: usize,
            _f: MemFlags,
        ) -> Result<(), MemoryRemappingError> {
            Ok(())
        }
        fn activate_handle(&self) -> AddressSpaceHandle {
            AddressSpaceHandle::new(PhysicalAddress::new(0), AddressSpaceTag::NONE)
        }
        fn zero_owned_frame(&self, _p: PageAlignedAddress) {}
        fn as_any(&self) -> &(dyn core::any::Any + 'static) {
            self
        }
    }

    fn nz(v: usize) -> NonZeroUsize {
        NonZeroUsize::new(v).unwrap()
    }

    fn region() -> Arc<MemoryRegion> {
        Arc::new(MemoryRegion::create_physical_device(
            PageAlignedAddress::from_usize(0x8000_0000).unwrap(),
            nz(PAGE),
            AccessMask::RW,
        ))
    }

    fn setup() -> (
        UserVmContext,
        Arc<TrackingMapper>,
        PageAlignedVirtualAddress,
    ) {
        let mapper = Arc::new(TrackingMapper::new());
        let mut alloc = UserVmAllocator::new(
            PageAlignedVirtualAddress::from_usize(ARENA).unwrap(),
            VirtualAddress::new(ARENA + 16 * PAGE),
        );
        let allocated = alloc
            .allocate(
                nz(PAGE),
                MappingTag {
                    flags: MemFlags::user_rw(),
                    region: region(),
                    grant: AccessMask::RW,
                    revocation: None,
                },
            )
            .unwrap();
        let base = allocated.base();
        let ctx = UserVmContext::new(mapper.clone(), Arc::new(MutexCell::new(alloc)));
        (ctx, mapper, base)
    }

    #[test]
    fn unmap_range_unmaps_and_frees() {
        let (ctx, mapper, base) = setup();
        ctx.unmap_range(base, nz(PAGE));

        assert_eq!(
            mapper.unmapped.with_lock(|u| u.clone()),
            vec![(base.as_usize(), PAGE)]
        );
        // Range вернулся в аллокатор: повторный free дал бы NotFound.
        ctx.allocator()
            .with_lock(|a| assert!(a.lookup(base, nz(PAGE)).is_err()));
    }

    #[test]
    fn unmap_range_is_idempotent() {
        let (ctx, mapper, base) = setup();
        ctx.unmap_range(base, nz(PAGE));
        // Повторный отзыв (двойной close / гонка) - no-op, без паники: unmap
        // отдаёт NotMapped, free - NotFound, оба трактуются как успех.
        ctx.unmap_range(base, nz(PAGE));
        // Второй unmap дошёл до mapper и был отклонён как NotMapped - в логе
        // только первый успешный вызов.
        assert_eq!(mapper.unmapped.with_lock(|u| u.len()), 1);
    }

    #[test]
    fn drain_releases_mappings_while_an_allocator_clone_survives() {
        let (ctx, mapper, base) = setup();
        let allocator = ctx.allocator().clone();

        ctx.drain();
        drop(ctx);

        assert_eq!(
            mapper.unmapped.with_lock(|u| u.clone()),
            vec![(base.as_usize(), PAGE)]
        );
        allocator.with_lock(|a| assert_eq!(a.live_count(), 0));
    }
}
