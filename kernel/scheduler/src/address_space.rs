use alloc::sync::Arc;

use memory::memory_mapper::{AddressSpaceFactory, AddressSpaceHandle, AsCreateError, MemoryMapper};

/// Адресное пространство процесса: Kernel (общий) или User (уникальный mapper).
pub enum AddressSpace {
    Kernel,
    User(Arc<dyn MemoryMapper + Send + Sync>),
}

impl AddressSpace {
    /// Возвращает shared kernel-AS для всех kernel-thread'ов.
    pub fn kernel() -> Arc<Self> {
        Arc::new(Self::Kernel)
    }

    /// Создаёт user-AS через фабрику; возвращает уникальный `Arc<Self>`.
    pub fn new_user(factory: &dyn AddressSpaceFactory) -> Result<Arc<Self>, AsCreateError> {
        let mapper = factory.create_user()?;
        Ok(Arc::new(Self::User(mapper)))
    }

    /// Capability для передачи в [`super::ArchContext::switch_address_space`]; `None` для kernel-AS.
    pub fn handle(&self) -> Option<AddressSpaceHandle> {
        match self {
            Self::Kernel => None,
            Self::User(mapper) => Some(mapper.activate_handle()),
        }
    }

    /// Mapper user-AS для kernel-side операций над user-страницами; `None` для kernel-AS.
    pub fn mapper(&self) -> Option<&(dyn MemoryMapper + Send + Sync)> {
        match self {
            Self::Kernel => None,
            Self::User(mapper) => Some(&**mapper),
        }
    }

    /// `Arc` user-mapper'а для использования вне scheduler-lock; `None` для kernel-AS.
    pub fn mapper_arc(&self) -> Option<Arc<dyn MemoryMapper + Send + Sync>> {
        match self {
            Self::Kernel => None,
            Self::User(mapper) => Some(mapper.clone()),
        }
    }
}

impl core::fmt::Debug for AddressSpace {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Kernel => f.write_str("AddressSpace::Kernel"),
            Self::User(_) => f.write_str("AddressSpace::User(..)"),
        }
    }
}
