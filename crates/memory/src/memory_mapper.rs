use alloc::sync::Arc;
use core::fmt::{Display, Formatter};

use crate::{
    MemFlags,
    physical_address::{PageAlignedAddress, PhysicalAddress},
    virtual_address::PageAlignedVirtualAddress,
};

/// Непрозрачный per-AS тег, выдаваемый платформой.
///
/// Сама платформа решает, что хранить внутри: monotonic generation + ASID/PCID
/// или единственное `0` для арх, где тегов нет. Архитектурно-независимый код
/// никогда не интерпретирует значение, только переносит его между mapper-ом и
/// `ArchContext::switch_address_space`.
#[repr(transparent)]
#[derive(Copy, Clone, Eq, PartialEq, Debug)]
pub struct AddressSpaceTag(pub u64);

impl AddressSpaceTag {
    /// Тег, гарантированно соответствующий "пустому" / kernel-only AS.
    pub const NONE: Self = Self(0);

    #[must_use]
    pub const fn raw(self) -> u64 {
        self.0
    }
}

/// Снимок состояния AS, потребный планировщику для активации AS:
/// корень таблиц трансляции + платформенный тег.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct AddressSpaceHandle {
    pub root: PhysicalAddress,
    pub tag: AddressSpaceTag,
}

impl AddressSpaceHandle {
    #[must_use]
    pub const fn new(root: PhysicalAddress, tag: AddressSpaceTag) -> Self {
        Self { root, tag }
    }
}

/// Ошибки при маппинге памяти.
#[derive(Debug, Clone, PartialEq, Eq)]
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
#[derive(Debug, Clone, Eq, PartialEq)]
pub enum MemoryUnmappingError {
    /// В диапазоне есть незамапленная страница. Уже снятые с маппинга страницы
    /// **не** возвращаются - частичный unmap валиден от вызывающего.
    NotMapped,
    /// На пути встретился block-mapping (1G/2M); split не поддерживается.
    UnsupportedBlockMapping,
    /// Размер диапазона не кратен 4 КБ.
    MisalignedRange,
}

impl Display for MemoryUnmappingError {
    fn fmt(&self, f: &mut Formatter<'_>) -> core::fmt::Result {
        match self {
            MemoryUnmappingError::NotMapped => f.write_str("Range contains an unmapped page"),
            MemoryUnmappingError::UnsupportedBlockMapping => {
                f.write_str("Block mapping (1G/2M) cannot be unmapped without split")
            }
            MemoryUnmappingError::MisalignedRange => f.write_str("Range size is not 4K aligned"),
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
    /// Маппит `page_count` свежевыделенных 4К-фреймов на VA-диапазон
    /// `[va, va + page_count * 4K)` со флагами `flags`.
    ///
    /// Mapper аллоцирует фреймы из своего frame-аллокатора и владеет ими: при
    /// дропе AS все выделенные здесь фреймы возвращаются обратно. Перевыделение
    /// (повторный `map` на ту же VA) запрещено - возвращается
    /// [`MemoryMappingError::AlreadyMapped`].
    ///
    /// `init` копируется в начало региона - байт `i` оказывается в `va + i`.
    /// Хвост `[init.len(), page_count * 4K)` остаётся занулённым (страницы
    /// свежие). Длина `init` должна быть `<= page_count * 4K`.
    ///
    /// **Атомарность leaf-страниц.** При ошибке посередине цикла уже
    /// замапленные leaf-страницы откатываются (PTE обнуляются, фреймы
    /// возвращаются аллокатору). После возврата `Err` повторный `map` по
    /// тому же VA-диапазону безопасен - `AlreadyMapped` не возникнет.
    /// Промежуточные L1/L2/L3-таблицы, аллоцированные walk'ом, могут
    /// остаться в дереве - они освобождаются при Drop AS и не мешают
    /// повторному `map`.
    fn map(
        &self,
        va: PageAlignedVirtualAddress,
        page_count: usize,
        init: &[u8],
        flags: MemFlags,
    ) -> Result<(), MemoryMappingError>;

    ///Создает связь между исходным виртуальным адресом и физическим адресом.
    fn map_exact(
        &self,
        source_address: PageAlignedVirtualAddress,
        target_address: PageAlignedAddress,
        size: usize,
        mem_flags: MemFlags,
    ) -> Result<(), MemoryMappingError>;

    /// Снимает маппинг 4К-страниц в диапазоне `[address, address + size)`.
    /// Страницы, замапленные через [`Self::map`] (фрейм аллоцирован
    /// `FrameAllocator`-ом mapper'а), возвращаются обратно. Страницы,
    /// замапленные через [`Self::map_exact`] (PA приходит от вызывающего -
    /// MMIO, image, identity), **не** возвращаются: их PA не принадлежит
    /// аллокатору фреймов.
    ///
    /// `size` должен быть кратен размеру страницы (4 КБ). Все страницы
    /// диапазона должны быть замаплены 4К-страницами; при первой
    /// незамапленной - [`MemoryUnmappingError::NotMapped`]. Уже снятые с
    /// маппинга страницы при этом **не** возвращаются.
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

    /// Возвращает handle на AS, лениво аллоцируя
    /// платформенный тег. Вызывается на пути активации AS планировщиком.
    fn activate_handle(&self) -> AddressSpaceHandle;

    /// Доступ к конкретной реализации через `Any`-downcast.
    ///
    /// Используется кодом, которому нужны платформенные API сверх общего
    /// контракта `MemoryMapper` (например, инспекция leaf-дескрипторов на
    /// конкретной архитектуре). Все реализации обязаны вернуть `self`.
    fn as_any(&self) -> &(dyn core::any::Any + 'static);
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
    fn create_user(&self) -> Result<Arc<dyn MemoryMapper + Send + Sync>, AsCreateError>;
}

#[cfg(test)]
mod tests {
    extern crate std;

    use std::string::ToString;

    use super::*;

    #[test]
    fn mapping_error_display_round_trip() {
        assert_eq!(MemoryMappingError::OutOfMemory.to_string(), "Out of memory");
        assert_eq!(
            MemoryMappingError::AlreadyMapped.to_string(),
            "Source address is already mapped"
        );
        assert_eq!(
            MemoryMappingError::VirtualMappingError.to_string(),
            "Virtual memory mapping error"
        );
    }

    #[test]
    fn unmapping_error_display_distinguishes_variants() {
        let strings = [
            MemoryUnmappingError::NotMapped.to_string(),
            MemoryUnmappingError::UnsupportedBlockMapping.to_string(),
            MemoryUnmappingError::MisalignedRange.to_string(),
        ];
        for (i, a) in strings.iter().enumerate() {
            for (j, b) in strings.iter().enumerate() {
                if i != j {
                    assert_ne!(a, b, "variants must produce distinct messages");
                }
            }
        }
    }

    #[test]
    fn unmapping_error_eq_and_clone() {
        let err = MemoryUnmappingError::NotMapped;
        assert_eq!(err.clone(), MemoryUnmappingError::NotMapped);
        assert_ne!(err, MemoryUnmappingError::UnsupportedBlockMapping);
        assert_ne!(err, MemoryUnmappingError::MisalignedRange);
    }

    #[test]
    fn remapping_error_display_distinguishes_variants() {
        let strings = [
            MemoryRemappingError::NotMapped.to_string(),
            MemoryRemappingError::UnsupportedBlockMapping.to_string(),
            MemoryRemappingError::MisalignedRange.to_string(),
        ];
        for (i, a) in strings.iter().enumerate() {
            for (j, b) in strings.iter().enumerate() {
                if i != j {
                    assert_ne!(a, b);
                }
            }
        }
    }
}
