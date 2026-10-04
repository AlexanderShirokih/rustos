use alloc::{boxed::Box, format, sync::Arc};
use core::num::NonZeroUsize;

use drivers_common::services::mmio::{
    CleanupCallback, MmioAddress, MmioBound, MmioMapError, MmioService,
};
use kbase::{LockCell, MutexCell};
use memory::{
    AccessMask, MemFlags, MemoryRegion, PAGE_SIZE,
    mem_flags::{AccessMode, Owners, UncachedPermission},
    memory_mapper::MemoryMapper,
    physical_address::PageAlignedAddress,
    range_allocator::RangeAllocator,
    virtual_address::{PageAlignedVirtualAddress, VirtualAddress},
};

pub type KernelMmioVaAllocator = MutexCell<RangeAllocator<()>>;

pub struct MmioServiceImpl {
    memory_mapper: &'static (dyn MemoryMapper + Send + Sync),
    va_allocator: Arc<KernelMmioVaAllocator>,
}

impl MmioServiceImpl {
    /// Создаёт сервис над диапазоном `[arena_base, arena_base + arena_size)`
    /// kernel-VA. Диапазон должен быть зарезервирован под MMIO-маппинги и
    /// не пересекаться ни с линейной картой PA->VA, ни с heap-ареной.
    pub fn new(
        memory_mapper: &'static (dyn MemoryMapper + Send + Sync),
        arena_base: PageAlignedVirtualAddress,
        arena_size: NonZeroUsize,
    ) -> Self {
        let arena_end = VirtualAddress::new(arena_base.as_usize().saturating_add(arena_size.get()));
        Self {
            memory_mapper,
            va_allocator: Arc::new(MutexCell::new(RangeAllocator::new(arena_base, arena_end))),
        }
    }
}

impl MmioService for MmioServiceImpl {
    fn map_mmio(
        &self,
        address: MmioAddress,
        permissions: Owners<UncachedPermission>,
    ) -> Result<MmioBound, MmioMapError> {
        let target_address = PageAlignedAddress::from_usize(address.base())
            .ok_or_else(|| MmioMapError("Mmio address is not aligned to 4K boundary".into()))?;
        let raw_size = NonZeroUsize::new(address.size())
            .ok_or_else(|| MmioMapError("Mmio region size must be non-zero".into()))?;

        // `pages * PAGE_SIZE` может переполниться при близком к usize::MAX
        // размере региона - тогда округление вверх некорректно; отвергаем.
        let pages = raw_size.get().div_ceil(PAGE_SIZE.get());
        let size = NonZeroUsize::new(pages)
            .and_then(|pages| pages.checked_mul(PAGE_SIZE))
            .ok_or_else(|| MmioMapError("Mmio region size overflows when rounded up".into()))?;

        let source_address = self
            .va_allocator
            .with_lock(|alloc| alloc.allocate(size, ()))
            .map_err(|err| MmioMapError(format!("VA arena: {err}")))?
            .base();

        let region = Arc::new(MemoryRegion::create_physical_device(
            target_address,
            size,
            access_mask_for_device(permissions),
        ));

        if let Err(err) = region.install(
            self.memory_mapper,
            source_address,
            MemFlags::Device(permissions),
        ) {
            let _ = self
                .va_allocator
                .with_lock(|alloc| alloc.free(source_address, size));
            return Err(MmioMapError(format!("Mapping error: {err}")));
        }

        let mapper = self.memory_mapper;
        let region_for_cleanup = region.clone();
        let va_allocator = self.va_allocator.clone();
        let cleanup: Box<CleanupCallback> = Box::new(
            move |virtual_address: PageAlignedVirtualAddress, _size: usize| {
                let _ = region_for_cleanup.uninstall(mapper, virtual_address);
                let _ = va_allocator.with_lock(|alloc| alloc.free(virtual_address, size));
            },
        );

        Ok(MmioBound::new(address, source_address, cleanup))
    }
}

fn access_mask_for_device(permissions: Owners<UncachedPermission>) -> AccessMask {
    match (permissions.kernel.access, permissions.user.access) {
        (AccessMode::Writable, _) | (_, AccessMode::Writable) => AccessMask::RW,
        (AccessMode::Readonly, _) | (_, AccessMode::Readonly) => AccessMask::R,
        _ => AccessMask::NONE,
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use memory::testing::{Call, MapExactCall, PageMapper, UnmapCall};

    use super::*;

    const ARENA_BASE: usize = 0xffff_fffc_0000_0000;
    const ARENA_SIZE: usize = 0x1_0000_0000;

    fn make_service(mapper: &'static PageMapper) -> MmioServiceImpl {
        MmioServiceImpl::new(
            mapper,
            PageAlignedVirtualAddress::from_usize(ARENA_BASE).unwrap(),
            NonZeroUsize::new(ARENA_SIZE).unwrap(),
        )
    }

    #[test]
    fn map_mmio_allocates_va_from_arena_independent_of_pa() {
        let mapper = Box::leak(Box::new(PageMapper::new()));
        let service = make_service(mapper);

        let address = MmioAddress::new(0x0c17_0000, 0x1000).unwrap();
        let bound = service
            .map_mmio(
                address,
                Owners::<UncachedPermission>::kernel(UncachedPermission::writable()),
            )
            .expect("mmio mapping must succeed");

        let calls = mapper.calls();
        assert_eq!(calls.len(), 1);
        let Call::MapExact(MapExactCall {
            va,
            pa,
            size,
            flags,
        }) = calls[0].clone()
        else {
            panic!("mmio mapping must use map_exact");
        };

        assert_eq!(va, ARENA_BASE, "first MMIO VA must equal arena base");
        assert_eq!(pa, 0x0c17_0000);
        assert_eq!(size, 0x1000);
        match flags {
            MemFlags::Device(owners) => match owners.kernel.access {
                AccessMode::Writable => {}
                _ => panic!("kernel MMIO mapping must remain writable"),
            },
            MemFlags::Cached(_) | MemFlags::WriteCombining(_) => {
                panic!("mmio mapping must use device flags")
            }
        }
        drop(bound);

        assert_eq!(
            mapper.unmaps(),
            [UnmapCall {
                va: ARENA_BASE,
                size: 0x1000
            }]
        );
    }

    #[test]
    fn map_mmio_rounds_sub_page_size_up_to_page() {
        let mapper = Box::leak(Box::new(PageMapper::new()));
        let service = make_service(mapper);

        let address = MmioAddress::new(0x0c16_f000, 0x200).unwrap();
        let bound = service
            .map_mmio(
                address,
                Owners::<UncachedPermission>::kernel(UncachedPermission::writable()),
            )
            .expect("mmio mapping must succeed");

        let maps = mapper.map_exacts();
        assert_eq!(maps.len(), 1);
        assert_eq!(
            maps[0].size,
            PAGE_SIZE.get(),
            "sub-page MMIO must round up to 4K"
        );

        drop(bound);

        let unmaps = mapper.unmaps();
        assert_eq!(unmaps.len(), 1);
        assert_eq!(unmaps[0].size, PAGE_SIZE.get());
    }

    #[test]
    fn map_mmio_returns_va_to_arena_after_unbind() {
        let mapper = Box::leak(Box::new(PageMapper::new()));
        let service = make_service(mapper);

        let address = MmioAddress::new(0x0c17_0000, 0x1000).unwrap();
        let first = service
            .map_mmio(
                address,
                Owners::<UncachedPermission>::kernel(UncachedPermission::writable()),
            )
            .expect("first mmio mapping must succeed");
        drop(first);

        let second = service
            .map_mmio(
                address,
                Owners::<UncachedPermission>::kernel(UncachedPermission::writable()),
            )
            .expect("second mmio mapping must reuse freed VA");

        let maps = mapper.map_exacts();
        assert_eq!(maps.len(), 2);
        assert_eq!(
            maps[1].va, ARENA_BASE,
            "freed VA must be reused for the next allocation"
        );
        drop(second);
    }

    fn user_owners(access: AccessMode) -> Owners<UncachedPermission> {
        Owners {
            kernel: UncachedPermission::default(),
            user: UncachedPermission { access },
        }
    }

    #[test]
    fn map_mmio_rolls_back_va_when_install_fails() {
        let mapper = Box::leak(Box::new(PageMapper::new()));
        mapper.fail_map_exact_after(0);
        let service = MmioServiceImpl::new(
            mapper,
            PageAlignedVirtualAddress::from_usize(ARENA_BASE).unwrap(),
            NonZeroUsize::new(ARENA_SIZE).unwrap(),
        );

        let address = MmioAddress::new(0x0c17_0000, 0x1000).unwrap();
        let perms = Owners::<UncachedPermission>::kernel(UncachedPermission::writable());

        let Err(err) = service.map_mmio(address, perms) else {
            panic!("install must fail");
        };
        assert!(format!("{err}").contains("Mapping error"));

        // Откат вернул VA в арену. Следующая попытка снова выделяет тот же стартовый VA.
        assert!(
            service.map_mmio(address, perms).is_err(),
            "install must fail again"
        );
        let remaining = service.va_allocator.with_lock(|alloc| {
            alloc
                .lookup(
                    PageAlignedVirtualAddress::from_usize(ARENA_BASE).unwrap(),
                    PAGE_SIZE,
                )
                .map(|_| ())
        });
        assert_eq!(
            remaining,
            Err(memory::range_allocator::RangeError::NotFound),
            "no VA must remain reserved after rolled-back installs"
        );
    }

    #[test]
    fn map_mmio_rejects_unaligned_address() {
        let mapper = Box::leak(Box::new(PageMapper::new()));
        let service = make_service(mapper);
        // 0x0c17_0001 не выровнен на 4К.
        let address = MmioAddress::new(0x0c17_0001, 0x1000).unwrap();
        let Err(err) = service.map_mmio(
            address,
            Owners::<UncachedPermission>::kernel(UncachedPermission::writable()),
        ) else {
            panic!("unaligned MMIO base must be rejected");
        };
        assert!(format!("{err}").contains("not aligned"));
        assert!(mapper.calls().is_empty());
    }

    #[test]
    fn map_mmio_rejects_zero_size() {
        let mapper = Box::leak(Box::new(PageMapper::new()));
        let service = make_service(mapper);
        let address = MmioAddress::new(0x0c17_0000, 0).unwrap();
        let Err(err) = service.map_mmio(
            address,
            Owners::<UncachedPermission>::kernel(UncachedPermission::writable()),
        ) else {
            panic!("zero-size MMIO must be rejected");
        };
        assert!(format!("{err}").contains("non-zero"));
        assert!(mapper.calls().is_empty());
    }

    #[test]
    fn access_mask_for_device_covers_kernel_user_readonly_and_none() {
        // Любой Writable (kernel или user) -> RW.
        assert_eq!(
            access_mask_for_device(Owners::<UncachedPermission>::kernel(
                UncachedPermission::writable()
            ))
            .bits(),
            AccessMask::RW.bits()
        );
        assert_eq!(
            access_mask_for_device(user_owners(AccessMode::Writable)).bits(),
            AccessMask::RW.bits()
        );
        // Readonly без Writable -> R.
        assert_eq!(
            access_mask_for_device(Owners::<UncachedPermission>::kernel(
                UncachedPermission::readonly()
            ))
            .bits(),
            AccessMask::R.bits()
        );
        assert_eq!(
            access_mask_for_device(user_owners(AccessMode::Readonly)).bits(),
            AccessMask::R.bits()
        );
        // Ни Writable, ни Readonly (оба None) -> NONE.
        assert_eq!(
            access_mask_for_device(user_owners(AccessMode::None)).bits(),
            AccessMask::NONE.bits()
        );
    }
}
