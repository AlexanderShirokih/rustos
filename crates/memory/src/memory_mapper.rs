use alloc::boxed::Box;
use core::fmt::{Display, Formatter};

use crate::{
    MemFlags,
    physical_address::{PageAlignedAddress, PhysicalAddress},
    virtual_address::PageAlignedVirtualAddress,
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

/// Ошибки при перемаппинге уже замапленных страниц.
#[derive(Debug, Clone, Eq, PartialEq)]
pub enum MemoryRemappingError {
    /// В диапазоне есть незамапленная страница.
    NotMapped,
    /// На пути встретился block-mapping (1G/2M); split не поддерживается.
    UnsupportedBlockMapping,
    /// Размер диапазона не кратен 4 КБ.
    MisalignedRange,
}

impl Display for MemoryRemappingError {
    fn fmt(&self, f: &mut Formatter<'_>) -> core::fmt::Result {
        match self {
            MemoryRemappingError::NotMapped => f.write_str("Range contains an unmapped page"),
            MemoryRemappingError::UnsupportedBlockMapping => {
                f.write_str("Block mapping (1G/2M) cannot be remapped without split")
            }
            MemoryRemappingError::MisalignedRange => f.write_str("Range size is not 4K aligned"),
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

    /// Меняет флаги уже замапленного 4К-диапазона на `new_flags` без перевыделения фреймов.
    ///
    /// `size` должен быть кратен размеру страницы (4 КБ); диапазон должен быть полностью
    /// замаплен 4К-страницами. На L1/L2 block-mappings возвращается
    /// [`MemoryRemappingError::UnsupportedBlockMapping`].
    ///
    /// При ошибке посередине диапазона уже применённые обновления НЕ откатываются -
    /// вызывающий должен передавать диапазоны, в корректности которых уверен.
    fn remap(
        &self,
        start_address: PageAlignedVirtualAddress,
        size: usize,
        new_flags: MemFlags,
    ) -> Result<(), MemoryRemappingError>;

    /// Физический адрес корня таблиц трансляции, которым владеет mapper.
    /// Scheduler читает это значение, чтобы записать `TTBR0_EL1` при
    /// переключении user-AS.
    fn root_pa(&self) -> PhysicalAddress;

    /// Диагностика для qemu-тестов: возвращает сырое значение leaf-дескриптора
    /// для `address`. `None` - если страница не замаплена либо лежит в block-mapping.
    #[cfg(feature = "qemu-tests")]
    fn query_leaf_raw(&self, address: PageAlignedVirtualAddress) -> Option<u64>;
}

/// Ошибки создания нового user-AS через [`AddressSpaceFactory`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AsCreateError {
    /// Не удалось выделить фрейм под корень таблиц.
    OutOfMemory,
}

impl Display for AsCreateError {
    fn fmt(&self, f: &mut Formatter<'_>) -> core::fmt::Result {
        match self {
            AsCreateError::OutOfMemory => f.write_str("Out of memory"),
        }
    }
}

/// Фабрика user-адресных пространств.
///
/// Создаёт пустой `MemoryMapper` с собственным L0-root, выделенным из
/// physical frame allocator. Используется scheduler-ом при spawn user-thread.
pub trait AddressSpaceFactory: Send + Sync {
    fn create_user(&self) -> Result<Box<dyn MemoryMapper + Send + Sync>, AsCreateError>;
}
