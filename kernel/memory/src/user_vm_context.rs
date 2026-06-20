use alloc::sync::Arc;

use collections::MutexCell;

use crate::{memory_mapper::MemoryMapper, user_vm_allocator::UserVmAllocator};

/// Снимок per-process user-памяти, передаваемый syscall-handler-ам.
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

    /// Клонирует `Arc` на mapper - нужен слою port-IPC, чтобы хранить
    /// транспорт заблокированного потока (кросс-AS rendezvous).
    pub fn mapper_arc(&self) -> Arc<dyn MemoryMapper + Send + Sync> {
        self.mapper.clone()
    }

    pub fn allocator(&self) -> &Arc<MutexCell<UserVmAllocator>> {
        &self.allocator
    }
}
