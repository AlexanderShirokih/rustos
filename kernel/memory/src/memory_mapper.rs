use alloc::sync::Arc;
use core::fmt::{Display, Formatter};

use crate::{
    MemFlags,
    physical_address::{PageAlignedAddress, PhysicalAddress},
    virtual_address::{PageAlignedVirtualAddress, VirtualAddress},
};

/// Непрозрачный per-AS тег, выдаваемый платформой.
///
/// Сама платформа решает, что хранить внутри: monotonic generation + ASID/PCID
/// или единственное `0` для арх, где теги отсутствуют. Архитектурно-независимый код
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
    /// Диапазон непредставим в `usize`, либо трансляция отказала по причине, для которой
    /// нет отдельного варианта.
    VirtualMappingError,
    /// Аллокатор фреймов отказал при выделении фрейма для страницы или таблицы трансляции.
    OutOfMemory,
    /// Целевой VA уже замаплен - повторный `map` на тот же диапазон запрещён.
    AlreadyMapped,
    /// `init` длиннее маппируемого диапазона.
    InitExceedsMapping,
}

impl Display for MemoryMappingError {
    fn fmt(&self, f: &mut Formatter<'_>) -> core::fmt::Result {
        match self {
            MemoryMappingError::VirtualMappingError => f.write_str("Virtual memory mapping error"),
            MemoryMappingError::OutOfMemory => f.write_str("Out of memory"),
            MemoryMappingError::AlreadyMapped => f.write_str("Source address is already mapped"),
            MemoryMappingError::InitExceedsMapping => {
                f.write_str("Init bytes are longer than the mapped range")
            }
        }
    }
}

/// Ошибки при размаппинге памяти.
#[derive(Debug, Clone, Eq, PartialEq)]
pub enum MemoryUnmappingError {
    /// В диапазоне есть незамапленная страница. Уже снятые с маппинга страницы
    /// **не** возвращаются - вызывающий должен считать частичный unmap валидным.
    NotMapped,
    /// Запрошен частичный unmap внутри block-mapping. Split не поддерживается.
    UnsupportedBlockMapping,
    /// Размер диапазона не кратен 4 КБ.
    MisalignedRange,
}

impl Display for MemoryUnmappingError {
    fn fmt(&self, f: &mut Formatter<'_>) -> core::fmt::Result {
        match self {
            MemoryUnmappingError::NotMapped => f.write_str("Range contains an unmapped page"),
            MemoryUnmappingError::UnsupportedBlockMapping => {
                f.write_str("Partial block mapping cannot be unmapped without split")
            }
            MemoryUnmappingError::MisalignedRange => f.write_str("Range size is not 4K aligned"),
        }
    }
}

/// Ошибки `copy_user_in`/`copy_user_out` через [`MemoryMapper`].
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum UserCopyError {
    /// В диапазоне есть VA без отображения.
    NotMapped,
    /// Отображение не разрешает требуемый доступ, имеет некэшируемые атрибуты либо
    /// физическая страница отсутствует в линейной карте ядра.
    AccessDenied,
}

impl Display for UserCopyError {
    fn fmt(&self, f: &mut Formatter<'_>) -> core::fmt::Result {
        match self {
            UserCopyError::NotMapped => f.write_str("Range contains an unmapped page"),
            UserCopyError::AccessDenied => f.write_str("User access denied for the requested op"),
        }
    }
}

/// Ошибки при перемаппинге уже замапленных страниц.
#[derive(Debug, Clone, Eq, PartialEq)]
pub enum MemoryRemappingError {
    /// В диапазоне есть незамапленная страница.
    NotMapped,
    /// На пути встретился block-mapping (1G/2M). Split не поддерживается.
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

/// Трейт маппера памяти для операций виртуальной памяти. Методы принимают `&self`.
/// Реализация отвечает за синхронизацию доступа к таблицам трансляции.
pub trait MemoryMapper {
    /// Маппит `page_count` свежевыделенных 4К-фреймов на VA-диапазон
    /// `[va, va + page_count * 4K)` со флагами `flags`.
    ///
    /// Mapper аллоцирует фреймы из своего frame-аллокатора и владеет ими. При
    /// дропе AS все выделенные здесь фреймы возвращаются обратно. Перевыделение
    /// (повторный `map` на ту же VA) запрещено - возвращается
    /// [`MemoryMappingError::AlreadyMapped`].
    ///
    /// `init` копируется в начало региона - байт `i` оказывается в `va + i`.
    /// Хвост `[init.len(), page_count * 4K)` остаётся занулённым (страницы
    /// свежие). `init` длиннее `page_count * 4K` отвергается как
    /// [`MemoryMappingError::InitExceedsMapping`] до первого маппинга.
    /// Непредставимый в `usize` размер или конец диапазона возвращает
    /// [`MemoryMappingError::VirtualMappingError`].
    ///
    /// **Атомарность leaf-страниц.** При ошибке посередине цикла уже
    /// замапленные leaf-страницы откатываются (PTE обнуляются, фреймы
    /// возвращаются аллокатору). После возврата `Err` повторный `map` по
    /// тому же VA-диапазону безопасен - `AlreadyMapped` не возникнет.
    /// Промежуточные L1/L2/L3-таблицы, аллоцированные при walk, могут
    /// остаться в дереве - они освобождаются при Drop AS и не мешают
    /// повторному `map`.
    fn map(
        &self,
        va: PageAlignedVirtualAddress,
        page_count: usize,
        init: &[u8],
        flags: MemFlags,
    ) -> Result<(), MemoryMappingError>;

    /// Маппит `[source_address, source_address + size)` VA на `target_address` PA явно,
    /// без выделения фрейма. `target_address` остаётся в собственности вызывающего -
    /// `unmap` PA не освобождает, в отличие от [`Self::map`]. Отказывает
    /// [`MemoryMappingError::AlreadyMapped`], если `source_address` уже замаплен.
    fn map_exact(
        &self,
        source_address: PageAlignedVirtualAddress,
        target_address: PageAlignedAddress,
        size: usize,
        mem_flags: MemFlags,
    ) -> Result<(), MemoryMappingError>;

    /// Снимает маппинг в диапазоне `[address, address + size)`.
    ///
    /// **Lifecycle PA**: фрейм возвращается в `FrameAllocator` этого mapper
    /// тогда и только тогда, когда он был выделен через [`Self::map`]. Весь
    /// остальной lifecycle PA контролирует caller - в частности, для PA,
    /// поданного через [`Self::map_exact`] (MMIO, identity, фреймы из
    /// внешнего владельца), `unmap` PA не освобождает. Регионы, владеющие
    /// фреймами на стороне caller (`MemoryRegion::Virtual`), возвращают свои
    /// фреймы при `Drop` региона.
    ///
    /// `size` должен быть кратен размеру страницы. Диапазон может покрывать
    /// 4К leaf-страницы и whole block leaf-маппинги. Частичный unmap внутри
    /// block leaf возвращает [`MemoryUnmappingError::UnsupportedBlockMapping`].
    /// При первой незамапленной странице - [`MemoryUnmappingError::NotMapped`].
    /// Уже снятые с маппинга страницы при этом **не** возвращаются.
    fn unmap(
        &self,
        address: PageAlignedVirtualAddress,
        size: usize,
    ) -> Result<(), MemoryUnmappingError>;

    /// Меняет флаги уже замапленного 4К-диапазона на `new_flags` без перевыделения фреймов.
    ///
    /// `size` должен быть кратен размеру страницы (4 КБ). Диапазон должен быть полностью
    /// замаплен 4К-страницами. На L1/L2 block-mappings возвращается
    /// [`MemoryRemappingError::UnsupportedBlockMapping`].
    ///
    /// При ошибке посередине диапазона уже применённые обновления не откатываются -
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

    /// Зануляет содержимое 4К-фрейма по PA через kernel-side доступ.
    ///
    /// Используется владельцами фреймов (например, `MemoryRegion::Virtual`)
    /// перед первым `map_exact`-маппингом в user-AS - иначе утечёт содержимое
    /// предыдущего владельца фрейма после reuse через `FrameAllocator`.
    /// Не пересекается с user-AS root: запись идёт через линейную карту
    /// физических фреймов (kernel-side), общую для всех mapper.
    fn zero_owned_frame(&self, pa: PageAlignedAddress);

    /// Копирует пользовательский диапазон `[va, va + dst.len())` в `dst`.
    /// Адрес `va` произволен. Диапазон может пересекать страницы.
    /// Функция проверяет право пользователя на чтение каждой страницы и обращается
    /// к физической памяти через кэшируемую линейную карту ядра. Адресное пространство
    /// пользователя может быть неактивным. Функция возвращает [`UserCopyError::AccessDenied`],
    /// если страница отсутствует в линейной карте либо имеет некэшируемые атрибуты.
    fn copy_user_in(&self, va: VirtualAddress, dst: &mut [u8]) -> Result<(), UserCopyError>;

    /// Копирует `src` в пользовательскую память по адресу `va`.
    /// Функция требует право пользователя на запись каждой страницы. Остальные условия
    /// совпадают с [`Self::copy_user_in`].
    fn copy_user_out(&self, va: VirtualAddress, src: &[u8]) -> Result<(), UserCopyError>;

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

    // Сообщения Display уходят в логи ядра и остаются
    // непустыми и различимыми между вариантами.
    #[test]
    fn mapping_error_display_messages() {
        assert_eq!(MemoryMappingError::OutOfMemory.to_string(), "Out of memory");
        assert_eq!(
            MemoryMappingError::AlreadyMapped.to_string(),
            "Source address is already mapped"
        );
        assert_eq!(
            MemoryMappingError::VirtualMappingError.to_string(),
            "Virtual memory mapping error"
        );
        assert_eq!(
            MemoryMappingError::InitExceedsMapping.to_string(),
            "Init bytes are longer than the mapped range"
        );
    }
}
