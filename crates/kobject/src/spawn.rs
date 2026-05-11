//! ABI-нейтральные описания для [`KernelRuntime::load_user_image_into`]
//! и [`KernelRuntime::start_user_process`].
//!
//! [`KernelRuntime::load_user_image_into`]: super::KernelRuntime::load_user_image_into
//! [`KernelRuntime::start_user_process`]: super::KernelRuntime::start_user_process

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
    runtime::UserThreadEntry,
};

/// Один сегмент образа, готовый к установке в child AS.
pub struct UserSegmentInstall {
    /// Базовый VA сегмента в child AS (4K-выровнен).
    pub va_base: PageAlignedVirtualAddress,
    /// Замапливаемый размер в байтах (4K-кратен; равен `region.size_bytes()`).
    pub mapped_size: usize,
    /// Регион, чьи фреймы будут установлены в child AS.
    pub region: Arc<MemoryRegion>,
    /// Флаги маппинга в child AS (R/W/X в user-режиме).
    pub flags: MemFlags,
}

/// Полное описание образа, которое scheduler установит в child AS.
pub struct UserImageInstall {
    pub segments: Vec<UserSegmentInstall>,
    /// PC первого user-инструкции.
    pub entry: VirtualAddress,
    /// Вершина user-стека (4K-выровнена).
    pub user_stack_top: VirtualAddress,
    /// Размер user-стека в байтах (4K-кратен).
    pub user_stack_size: usize,
    /// База диапазона user_vm-аллокатора (4K-выровнена).
    pub user_vm_base: PageAlignedVirtualAddress,
    /// Размер диапазона user_vm-аллокатора в байтах.
    pub user_vm_size: usize,
}

/// Параметры старта первого user-потока.
///
/// Handles перечисляются по `HandleId` в loader-таблице caller'а; scheduler
/// сам drain'ит их под scheduler-lock-ом после успешного создания thread'а,
/// поэтому любая ошибка до drain (включая `SpawnFailed`) оставляет handles
/// нетронутыми в loader-table с исходными идентификаторами.
pub struct UserStartSpec {
    pub entry: UserThreadEntry,
    /// Loader-таблица, из которой scheduler dranит bootstrap-handles.
    pub loader_handle_table: Arc<MutexCell<HandleTable>>,
    /// `HandleId`'ы bootstrap-handles в `loader_handle_table` (в исходном
    /// порядке). Все должны существовать и иметь `Rights::TRANSFER`.
    pub handle_ids: Vec<HandleId>,
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
