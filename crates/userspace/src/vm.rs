//! Снимок per-process user-памяти, передаваемый syscall-handler-ам user-VM.
//!
//! `UserVmAllocator` живёт в крейте [`memory`] (он архитектурно-независимый
//! и используется не только из userspace-подсистемы). Переэкспорт ниже даёт
//! одну точку поиска для пользователей `userspace`.

use alloc::sync::Arc;

use collections::MutexCell;
use memory::memory_mapper::MemoryMapper;
pub use memory::user_vm_allocator::UserVmAllocator;

/// Снимок per-process user-памяти, передаваемый syscall-handler-ам.
///
/// Оба поля - `Arc`-ы из `Process`, копирование пары - два clone'а Arc.
pub struct UserVmContext {
    mapper: Arc<dyn MemoryMapper + Send + Sync>,
    allocator: Arc<MutexCell<UserVmAllocator>>,
}

impl UserVmContext {
    pub fn new(
        mapper: Arc<dyn MemoryMapper + Send + Sync>,
        allocator: Arc<MutexCell<UserVmAllocator>>,
    ) -> Self {
        Self { mapper, allocator }
    }

    pub fn mapper(&self) -> &dyn MemoryMapper {
        &*self.mapper
    }

    pub fn allocator(&self) -> &Arc<MutexCell<UserVmAllocator>> {
        &self.allocator
    }
}
