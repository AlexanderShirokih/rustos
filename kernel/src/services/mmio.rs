use alloc::boxed::Box;
use alloc::format;
use drivers_common::services::mmio::{MmioAddress, MmioBound, MmioMapError, MmioService};
use memory::MemFlags;
use memory::mem_flags::{DeviceMemoryPermission, Owners};
use memory::memory_mapper::MemoryMapper;
use memory::physical_address::PageAlignedAddress;
use memory::virtual_address::PageAlignedVirtualAddress;

pub struct MmioServiceImpl {
    pub(crate) memory_mapper: &'static dyn MemoryMapper,
    pub(crate) linear_offset: PageAlignedVirtualAddress,
}

// SAFETY: Runtime memory mapper используется как глобальный сервис и должен быть
// безопасен для конкурентного доступа в рамках ядра.
unsafe impl Send for MmioServiceImpl {}
unsafe impl Sync for MmioServiceImpl {}

impl MmioService for MmioServiceImpl {
    fn map_mmio(
        &self,
        address: MmioAddress,
        permissions: Owners<DeviceMemoryPermission>,
    ) -> Result<MmioBound, MmioMapError> {
        let target_address = PageAlignedAddress::from_usize(address.base())
            .ok_or_else(|| MmioMapError("Mmio address is not aligned to 4K boundary".into()))?;

        let source_address =
            PageAlignedVirtualAddress::from_aligned_offset(target_address, self.linear_offset);

        let mapper = self.memory_mapper;

        mapper
            .map_exact(
                source_address,
                target_address,
                address.size(),
                MemFlags::Device(permissions),
            )
            .map_err(|err| MmioMapError(format!("Mapping error: {err}")))?;

        let cleanup = Box::new(
            move |virtual_address: PageAlignedVirtualAddress, size: usize| {
                let _ = mapper.unmap(virtual_address, size);
            },
        );

        Ok(MmioBound::new(address, source_address, cleanup))
    }
}
