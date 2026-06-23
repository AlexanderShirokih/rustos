use alloc::{sync::Arc, vec::Vec};

use collections::MutexCell;
use memory::{
    MemFlags, MemoryRegion,
    memory_mapper::MemoryMappingError,
    virtual_address::{PageAlignedVirtualAddress, VirtualAddress},
};

use super::{
    HandleTable,
    errors::{IpcError, SpawnError},
    handle::HandleId,
    resource::Resource,
    runtime::UserThreadEntry,
};

/// Сегмент образа, готовый к установке в child AS.
pub struct UserSegmentInstall {
    /// Базовый VA сегмента в child AS.
    pub va_base: PageAlignedVirtualAddress,
    /// Размер региона в байтах (равен `region.size_bytes()`).
    pub mapped_size: usize,
    /// Регион, чьи фреймы будут установлены в child AS.
    pub region: Arc<MemoryRegion>,
    /// Флаги маппинга в child AS.
    pub flags: MemFlags,
}

/// Описание образа, которое планировщик установит в child AS.
pub struct UserImageInstall {
    pub segments: Vec<UserSegmentInstall>,
    /// PC первого user-инструкции.
    pub entry: VirtualAddress,
    /// Вершина user-стека.
    pub user_stack_top: VirtualAddress,
    /// Размер user-стека в байтах (4K-кратен).
    pub user_stack_size: usize,
    /// База диапазона user_vm-аллокатора.
    pub user_vm_base: PageAlignedVirtualAddress,
    /// Размер диапазона user_vm-аллокатора в байтах.
    pub user_vm_size: usize,
}

/// Параметры старта первого user-потока.
pub struct UserStartSpec {
    pub entry: UserThreadEntry,
    pub loader_handle_table: Arc<MutexCell<HandleTable>>,
    pub handle_ids: Vec<HandleId>,
    pub metering_resource: Option<Arc<Resource>>,
}

/// Ошибки [`KernelRuntime::load_user_image_into`].
#[derive(Debug)]
pub enum LoadImageError {
    ProcessNotFound,
    /// Образ уже загружен или у процесса есть потоки/handle'ы.
    WrongState,
    /// Процесс создан в kernel-AS (без mapper-а user-страниц).
    NoUserAddressSpace,
    /// `user_vm_base + user_vm_size` переполняет `usize`.
    UserVmRangeOverflow,
    /// Ошибка установки PTE для сегмента или стека.
    MappingFailed(MemoryMappingError),
}

/// Ошибки [`KernelRuntime::start_user_process`]. Все варианты возвращаются
/// без модификации loader-таблицы.
#[derive(Debug)]
pub enum StartProcessError {
    ProcessNotFound,
    /// Процесс ещё не загружен или уже стартовал.
    WrongState,
    /// Один из `handle_ids` не найден, не имеет `Rights::TRANSFER` или
    /// дублируется в массиве.
    HandleValidationFailed(IpcError),
    /// Не удалось создать первый user-поток.
    SpawnFailed(SpawnError),
}
