use crate::physical_address::PageAlignedAddress;
use crate::virtual_address::PageAlignedVirtualAddress;

/// Типы ошибок при сбое маппинга памяти
#[derive(Debug, Clone)]
pub enum MemoryMappingError {
    VirtualMappingError,
    OutOfMemory,
    AlreadyMapped,
}

/// Трейт маппера памяти для операций виртуальной памяти
pub trait MemoryMapper {
    /// Отобразить физические фреймы в виртуальную память для кучи
    fn map_frames(
        &mut self,
        start_address: &PageAlignedVirtualAddress,
        size: usize,
    ) -> Result<(), MemoryMappingError>;

    fn map_exact(
        &mut self,
        source_address: &PageAlignedAddress,
        target_address: &PageAlignedVirtualAddress,
        size: usize,
        mem_flags: u64,
    ) -> Result<(), MemoryMappingError>;
}
