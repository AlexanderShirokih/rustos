use alloc::{boxed::Box, format};
use core::num::NonZeroUsize;

use drivers_common::services::mmio::{
    CleanupCallback, MmioAddress, MmioBound, MmioMapError, MmioService,
};
use memory::{
    AccessMask, MemFlags, MemoryRegion,
    mem_flags::{AccessMode, DeviceMemoryPermission, Owners},
    memory_mapper::MemoryMapper,
    physical_address::PageAlignedAddress,
    virtual_address::PageAlignedVirtualAddress,
};

pub struct MmioServiceImpl {
    pub(crate) memory_mapper: &'static (dyn MemoryMapper + Send + Sync),
    pub(crate) linear_offset: PageAlignedVirtualAddress,
}

impl MmioService for MmioServiceImpl {
    fn map_mmio(
        &self,
        address: MmioAddress,
        permissions: Owners<DeviceMemoryPermission>,
    ) -> Result<MmioBound, MmioMapError> {
        let target_address = PageAlignedAddress::from_usize(address.base())
            .ok_or_else(|| MmioMapError("Mmio address is not aligned to 4K boundary".into()))?;
        let size = NonZeroUsize::new(address.size())
            .ok_or_else(|| MmioMapError("Mmio region size must be non-zero".into()))?;

        let source_address =
            PageAlignedVirtualAddress::from_aligned_offset(target_address, self.linear_offset);
        let region = alloc::sync::Arc::new(MemoryRegion::create_physical(
            target_address,
            size,
            access_mask_for_device(permissions),
        ));

        region
            .install(
                self.memory_mapper,
                source_address,
                MemFlags::Device(permissions),
            )
            .map_err(|err| MmioMapError(format!("Mapping error: {err}")))?;

        let mapper = self.memory_mapper;
        let region_for_cleanup = region.clone();
        let cleanup: Box<CleanupCallback> = Box::new(
            move |virtual_address: PageAlignedVirtualAddress, size: usize| {
                debug_assert_eq!(size, region_for_cleanup.size_bytes());
                let _ = region_for_cleanup.uninstall(mapper, virtual_address);
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

    #[test]
    fn map_mmio_installs_physical_region_and_cleanup_uninstalls_it() {
        let mapper = Box::leak(Box::new(SpyMapper::new()));
        let linear_offset = PageAlignedVirtualAddress::from_usize(0xffff_0000_0000_0000).unwrap();
        let service = MmioServiceImpl {
            memory_mapper: mapper,
            linear_offset,
        };
        let address = MmioAddress::new(0x4000_0000, 0x2000).unwrap();

        let bound = service
            .map_mmio(
                address,
                Owners::<DeviceMemoryPermission>::kernel(DeviceMemoryPermission::writable()),
            )
            .expect("mmio mapping must succeed");

        let source_address =
            PageAlignedVirtualAddress::from_usize(linear_offset.as_usize() + 0x4000_0000).unwrap();
        let maps = mapper.maps.lock().unwrap();
        assert_eq!(maps.len(), 1);
        let first = maps[0];
        assert_eq!(first.va, source_address);
        assert_eq!(
            first.pa,
            PageAlignedAddress::from_usize(0x4000_0000).unwrap()
        );
        assert_eq!(first.size, 0x2000);
        match first.flags {
            MemFlags::Device(owners) => match owners.kernel.access {
                AccessMode::Writable => {}
                _ => panic!("kernel MMIO mapping must remain writable"),
            },
            _ => panic!("mmio mapping must use device flags"),
        }
        drop(maps);

        drop(bound);

        assert_eq!(
            *mapper.unmaps.lock().unwrap(),
            Vec::from([(source_address, 0x2000)])
        );
    }
}
