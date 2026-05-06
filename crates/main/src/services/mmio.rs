#![allow(unsafe_code)]

use alloc::{boxed::Box, format};

use drivers_common::services::mmio::{
    CleanupCallback, MmioAddress, MmioBound, MmioMapError, MmioService,
};
use memory::{
    MemFlags,
    mem_flags::{DeviceMemoryPermission, Owners},
    memory_mapper::MemoryMapper,
    physical_address::PageAlignedAddress,
    virtual_address::PageAlignedVirtualAddress,
};

pub struct MmioServiceImpl {
    pub(crate) memory_mapper: &'static dyn MemoryMapper,
    pub(crate) linear_offset: PageAlignedVirtualAddress,
}

// SAFETY: Runtime memory mapper используется как глобальный сервис и должен быть
// безопасен для конкурентного доступа в рамках ядра.
unsafe impl Send for MmioServiceImpl {}
// SAFETY: см. impl Send - глобальный mapper защищён внутренней синхронизацией,
// `linear_offset` неизменяемое значение, `&self`-API не имеет интероп. состояния.
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

        self.memory_mapper
            .map_exact(
                source_address,
                target_address,
                address.size(),
                MemFlags::Device(permissions),
            )
            .map_err(|err| MmioMapError(format!("Mapping error: {err}")))?;

        let mapper = self.memory_mapper;
        let cleanup: Box<CleanupCallback> = Box::new(
            move |virtual_address: PageAlignedVirtualAddress, size: usize| {
                let _ = mapper.unmap(virtual_address, size);
            },
        );

        Ok(MmioBound::new(address, source_address, cleanup))
    }
}
