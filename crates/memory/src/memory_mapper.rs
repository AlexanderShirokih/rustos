use core::fmt::{Display, Formatter};

use crate::{
    MemFlags, physical_address::PageAlignedAddress, virtual_address::PageAlignedVirtualAddress,
};

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

impl Display for MemoryMappingError {
    fn fmt(&self, f: &mut Formatter<'_>) -> core::fmt::Result {
        match self {
            MemoryMappingError::VirtualMappingError => f.write_str("Virtual memory mapping error"),
            MemoryMappingError::OutOfMemory => f.write_str("Out of memory"),
            MemoryMappingError::AlreadyMapped => f.write_str("Source address is already mapped"),
        }
    }
}

/// Ошибки при размаппинге памяти.
#[derive(Debug, Clone)]
pub enum MemoryUnmappingError {
    /// Операция размаппинга не поддерживается.
    Unsupported,
}

impl Display for MemoryUnmappingError {
    fn fmt(&self, f: &mut Formatter<'_>) -> core::fmt::Result {
        match self {
            MemoryUnmappingError::Unsupported => f.write_str("Unmapping is not supported"),
        }
    }
}

/// Трейт маппера памяти для операций виртуальной памяти.
pub trait MemoryMapper {
    /// Отображает физические фреймы в виртуальную память.
    fn map(
        &self,
        start_address: &PageAlignedVirtualAddress,
        size: usize,
    ) -> Result<(), MemoryMappingError>;

    ///Создает связь между исходным виртуальным адресом и физическим адресом.
    fn map_exact(
        &self,
        source_address: PageAlignedVirtualAddress,
        target_address: PageAlignedAddress,
        size: usize,
        mem_flags: MemFlags,
    ) -> Result<(), MemoryMappingError>;

    fn unmap(
        &self,
        address: PageAlignedVirtualAddress,
        size: usize,
    ) -> Result<(), MemoryUnmappingError>;
}
