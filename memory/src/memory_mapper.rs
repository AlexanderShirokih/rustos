use crate::physical_address::PageAlignedAddress;
use crate::virtual_address::PageAlignedVirtualAddress;

/// Ошибки при маппинге памяти.
#[derive(Debug, Clone)]
pub enum MemoryMappingError {
    /// Ошибка при создании виртуального маппинга.
    VirtualMappingError,
    /// Недостаточно памяти для маппинга.
    OutOfMemory,
    /// Адрес уже замаплен.
    AlreadyMapped,
}

/// Трейт маппера памяти для операций виртуальной памяти.
pub trait MemoryMapper {
    /// Отображает физические фреймы в виртуальную память.
    fn map(
        &self,
        start_address: &PageAlignedVirtualAddress,
        size: usize,
    ) -> Result<(), MemoryMappingError>;

    fn map_exact(
        &self,
        source_address: PageAlignedAddress,
        target_address: PageAlignedVirtualAddress,
        size: usize,
        mem_flags: u64,
    ) -> Result<(), MemoryMappingError>;
}
