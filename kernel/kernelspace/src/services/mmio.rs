use alloc::{boxed::Box, format, sync::Arc};
use core::num::NonZeroUsize;

use collections::{LockCell, MutexCell};
use drivers_common::services::mmio::{
    CleanupCallback, MmioAddress, MmioBound, MmioMapError, MmioService,
};
use memory::{
    AccessMask, MemFlags, MemoryRegion, PAGE_SIZE,
    mem_flags::{AccessMode, DeviceMemoryPermission, Owners},
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
        permissions: Owners<DeviceMemoryPermission>,
    ) -> Result<MmioBound, MmioMapError> {
        let target_address = PageAlignedAddress::from_usize(address.base())
            .ok_or_else(|| MmioMapError("Mmio address is not aligned to 4K boundary".into()))?;
        let raw_size = NonZeroUsize::new(address.size())
            .ok_or_else(|| MmioMapError("Mmio region size must be non-zero".into()))?;

        let pages = raw_size.get().div_ceil(PAGE_SIZE.get());
        // `pages * PAGE_SIZE` может переполниться при близком к usize::MAX
        // размере региона - тогда округление вверх некорректно; отвергаем.
        let size_bytes = pages
            .checked_mul(PAGE_SIZE.get())
            .ok_or_else(|| MmioMapError("Mmio region size overflows when rounded up".into()))?;
        let size = NonZeroUsize::new(size_bytes).expect("pages >= 1 since raw_size is non-zero");

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

fn access_mask_for_device(permissions: Owners<DeviceMemoryPermission>) -> AccessMask {
    match (permissions.kernel.access, permissions.user.access) {
        (AccessMode::Writable, _) | (_, AccessMode::Writable) => AccessMask::RW,
        (AccessMode::Readonly, _) | (_, AccessMode::Readonly) => AccessMask::R,
        _ => AccessMask::NONE,
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use std::{any::Any, sync::Mutex, vec::Vec};

    use memory::{
        memory_mapper::{
            AddressSpaceHandle, AddressSpaceTag, MemoryMappingError, MemoryRemappingError,
            MemoryUnmappingError,
        },
        physical_address::PhysicalAddress,
    };

    use super::*;

    #[derive(Clone, Copy)]
    struct MapCall {
        va: PageAlignedVirtualAddress,
        pa: PageAlignedAddress,
        size: usize,
        flags: MemFlags,
    }

    struct SpyMapper {
        maps: Mutex<Vec<MapCall>>,
        unmaps: Mutex<Vec<(PageAlignedVirtualAddress, usize)>>,
    }

    impl SpyMapper {
        fn new() -> Self {
            Self {
                maps: Mutex::new(Vec::new()),
                unmaps: Mutex::new(Vec::new()),
            }
        }
    }

    impl MemoryMapper for SpyMapper {
        fn map(
            &self,
            _va: PageAlignedVirtualAddress,
            _page_count: usize,
            _init: &[u8],
            _flags: MemFlags,
        ) -> Result<(), MemoryMappingError> {
            unreachable!("MmioServiceImpl must use MemoryRegion::install over physical regions")
        }

        fn map_exact(
            &self,
            source_address: PageAlignedVirtualAddress,
            target_address: PageAlignedAddress,
            size: usize,
            mem_flags: MemFlags,
        ) -> Result<(), MemoryMappingError> {
            self.maps.lock().unwrap().push(MapCall {
                va: source_address,
                pa: target_address,
                size,
                flags: mem_flags,
            });
            Ok(())
        }

        fn unmap(
            &self,
            address: PageAlignedVirtualAddress,
            size: usize,
        ) -> Result<(), MemoryUnmappingError> {
            self.unmaps.lock().unwrap().push((address, size));
            Ok(())
        }

        fn remap(
            &self,
            _start_address: PageAlignedVirtualAddress,
            _size: usize,
            _new_flags: MemFlags,
        ) -> Result<(), MemoryRemappingError> {
            unreachable!()
        }

        fn activate_handle(&self) -> AddressSpaceHandle {
            AddressSpaceHandle::new(PhysicalAddress::new(0), AddressSpaceTag::NONE)
        }

        fn zero_owned_frame(&self, _pa: PageAlignedAddress) {}

        fn as_any(&self) -> &(dyn Any + 'static) {
            self
        }
    }

    const ARENA_BASE: usize = 0xffff_fffc_0000_0000;
    const ARENA_SIZE: usize = 0x1_0000_0000;

    fn make_service(mapper: &'static SpyMapper) -> MmioServiceImpl {
        MmioServiceImpl::new(
            mapper,
            PageAlignedVirtualAddress::from_usize(ARENA_BASE).unwrap(),
            NonZeroUsize::new(ARENA_SIZE).unwrap(),
        )
    }

    #[test]
    fn map_mmio_allocates_va_from_arena_independent_of_pa() {
        let mapper = Box::leak(Box::new(SpyMapper::new()));
        let service = make_service(mapper);

        let address = MmioAddress::new(0x0c17_0000, 0x1000).unwrap();
        let bound = service
            .map_mmio(
                address,
                Owners::<DeviceMemoryPermission>::kernel(DeviceMemoryPermission::writable()),
            )
            .expect("mmio mapping must succeed");

        let maps = mapper.maps.lock().unwrap();
        assert_eq!(maps.len(), 1);
        let first = maps[0];

        assert_eq!(
            first.va,
            PageAlignedVirtualAddress::from_usize(ARENA_BASE).unwrap(),
            "first MMIO VA must equal arena base"
        );
        assert_eq!(
            first.pa,
            PageAlignedAddress::from_usize(0x0c17_0000).unwrap()
        );
        assert_eq!(first.size, 0x1000);
        match first.flags {
            MemFlags::Device(owners) => match owners.kernel.access {
                AccessMode::Writable => {}
                _ => panic!("kernel MMIO mapping must remain writable"),
            },
            MemFlags::Private(_) => panic!("mmio mapping must use device flags"),
        }
        drop(maps);

        drop(bound);

        assert_eq!(
            *mapper.unmaps.lock().unwrap(),
            Vec::from([(
                PageAlignedVirtualAddress::from_usize(ARENA_BASE).unwrap(),
                0x1000
            )])
        );
    }

    #[test]
    fn map_mmio_rounds_sub_page_size_up_to_page() {
        let mapper = Box::leak(Box::new(SpyMapper::new()));
        let service = make_service(mapper);

        let address = MmioAddress::new(0x0c16_f000, 0x200).unwrap();
        let bound = service
            .map_mmio(
                address,
                Owners::<DeviceMemoryPermission>::kernel(DeviceMemoryPermission::writable()),
            )
            .expect("mmio mapping must succeed");

        let maps = mapper.maps.lock().unwrap();
        assert_eq!(maps.len(), 1);
        assert_eq!(
            maps[0].size,
            PAGE_SIZE.get(),
            "sub-page MMIO must round up to 4K"
        );
        drop(maps);

        drop(bound);

        // unmap должен быть симметричен размеру install.
        let unmaps = mapper.unmaps.lock().unwrap();
        assert_eq!(unmaps.len(), 1);
        assert_eq!(unmaps[0].1, PAGE_SIZE.get());
    }

    #[test]
    fn map_mmio_returns_va_to_arena_after_unbind() {
        let mapper = Box::leak(Box::new(SpyMapper::new()));
        let service = make_service(mapper);

        let address = MmioAddress::new(0x0c17_0000, 0x1000).unwrap();
        let first = service
            .map_mmio(
                address,
                Owners::<DeviceMemoryPermission>::kernel(DeviceMemoryPermission::writable()),
            )
            .expect("first mmio mapping must succeed");
        let first_va = PageAlignedVirtualAddress::from_usize(ARENA_BASE).unwrap();
        drop(first);

        let second = service
            .map_mmio(
                address,
                Owners::<DeviceMemoryPermission>::kernel(DeviceMemoryPermission::writable()),
            )
            .expect("second mmio mapping must reuse freed VA");

        let maps = mapper.maps.lock().unwrap();
        assert_eq!(maps.len(), 2);
        assert_eq!(
            maps[1].va, first_va,
            "freed VA must be reused for the next allocation"
        );
        drop(maps);
        drop(second);
    }

    /// Mapper, чей `map_exact` всегда падает - для проверки отката VA.
    struct FailingMapper;

    impl MemoryMapper for FailingMapper {
        fn map(
            &self,
            _va: PageAlignedVirtualAddress,
            _pc: usize,
            _init: &[u8],
            _f: MemFlags,
        ) -> Result<(), MemoryMappingError> {
            unreachable!()
        }
        fn map_exact(
            &self,
            _va: PageAlignedVirtualAddress,
            _pa: PageAlignedAddress,
            _s: usize,
            _f: MemFlags,
        ) -> Result<(), MemoryMappingError> {
            Err(MemoryMappingError::OutOfMemory)
        }
        fn unmap(
            &self,
            _va: PageAlignedVirtualAddress,
            _s: usize,
        ) -> Result<(), MemoryUnmappingError> {
            Ok(())
        }
        fn remap(
            &self,
            _va: PageAlignedVirtualAddress,
            _s: usize,
            _f: MemFlags,
        ) -> Result<(), MemoryRemappingError> {
            Ok(())
        }
        fn activate_handle(&self) -> AddressSpaceHandle {
            AddressSpaceHandle::new(PhysicalAddress::new(0), AddressSpaceTag::NONE)
        }
        fn zero_owned_frame(&self, _pa: PageAlignedAddress) {}
        fn as_any(&self) -> &(dyn Any + 'static) {
            self
        }
    }

    fn user_owners(access: AccessMode) -> Owners<DeviceMemoryPermission> {
        Owners {
            kernel: DeviceMemoryPermission::default(),
            user: DeviceMemoryPermission { access },
        }
    }

    #[test]
    fn map_mmio_rolls_back_va_when_install_fails() {
        let mapper = Box::leak(Box::new(FailingMapper));
        let service = MmioServiceImpl::new(
            mapper,
            PageAlignedVirtualAddress::from_usize(ARENA_BASE).unwrap(),
            NonZeroUsize::new(ARENA_SIZE).unwrap(),
        );

        let address = MmioAddress::new(0x0c17_0000, 0x1000).unwrap();
        let perms = Owners::<DeviceMemoryPermission>::kernel(DeviceMemoryPermission::writable());

        let Err(err) = service.map_mmio(address, perms) else {
            panic!("install must fail");
        };
        assert!(format!("{err}").contains("Mapping error"));

        // Откат вернул VA в арену: следующая попытка снова выделяет тот же стартовый VA.
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
        let mapper = Box::leak(Box::new(SpyMapper::new()));
        let service = make_service(mapper);
        // 0x0c17_0001 не выровнен на 4К.
        let address = MmioAddress::new(0x0c17_0001, 0x1000).unwrap();
        let Err(err) = service.map_mmio(
            address,
            Owners::<DeviceMemoryPermission>::kernel(DeviceMemoryPermission::writable()),
        ) else {
            panic!("unaligned MMIO base must be rejected");
        };
        assert!(format!("{err}").contains("not aligned"));
        assert!(mapper.maps.lock().unwrap().is_empty());
    }

    #[test]
    fn map_mmio_rejects_zero_size() {
        let mapper = Box::leak(Box::new(SpyMapper::new()));
        let service = make_service(mapper);
        let address = MmioAddress::new(0x0c17_0000, 0).unwrap();
        let Err(err) = service.map_mmio(
            address,
            Owners::<DeviceMemoryPermission>::kernel(DeviceMemoryPermission::writable()),
        ) else {
            panic!("zero-size MMIO must be rejected");
        };
        assert!(format!("{err}").contains("non-zero"));
        assert!(mapper.maps.lock().unwrap().is_empty());
    }

    #[test]
    fn access_mask_for_device_covers_kernel_user_readonly_and_none() {
        // Любой Writable (kernel или user) -> RW.
        assert_eq!(
            access_mask_for_device(Owners::<DeviceMemoryPermission>::kernel(
                DeviceMemoryPermission::writable()
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
            access_mask_for_device(Owners::<DeviceMemoryPermission>::kernel(
                DeviceMemoryPermission::readonly()
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
