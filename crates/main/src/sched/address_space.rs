use alloc::sync::Arc;

use memory::memory_mapper::{AddressSpaceFactory, AddressSpaceHandle, AsCreateError, MemoryMapper};

/// Адресное пространство процесса.
///
/// Kernel-thread'ы шарят единый `Kernel`-вариант. User-вариант владеет
/// собственным mapper-ом с уникальным root: scheduler активирует его
/// через [`super::ArchContext::switch_address_space`] при переключении
/// на thread этого процесса.
pub enum AddressSpace {
    Kernel,
    User(Arc<dyn MemoryMapper + Send + Sync>),
}

impl AddressSpace {
    /// Общий kernel-AS для всех kernel-thread'ов. Возвращает один и тот же
    /// `Arc` через clone не требует - вызывающий получает свежий `Arc`,
    /// scheduler хранит один экземпляр в `kernel_address_space`.
    pub fn kernel() -> Arc<Self> {
        Arc::new(Self::Kernel)
    }

    /// Создаёт user-AS через фабрику. Возвращает уникальный `Arc<Self>`
    /// с собственным L0-root.
    pub fn new_user(factory: &dyn AddressSpaceFactory) -> Result<Arc<Self>, AsCreateError> {
        let mapper = factory.create_user()?;
        Ok(Arc::new(Self::User(mapper)))
    }

    /// Handle, передаваемый
    /// [`super::ArchContext::switch_address_space`]. `None` - kernel-AS.
    /// Может лениво аллоцировать платформенный тег, поэтому вызывать только
    /// на пути активации.
    pub fn handle(&self) -> Option<AddressSpaceHandle> {
        match self {
            Self::Kernel => None,
            Self::User(mapper) => Some(mapper.activate_handle()),
        }
    }

    /// Mapper user-AS для kernel-side операций над user-страницами.
    /// Для kernel-AS возвращает `None`.
    pub fn mapper(&self) -> Option<&(dyn MemoryMapper + Send + Sync)> {
        match self {
            Self::Kernel => None,
            Self::User(mapper) => Some(&**mapper),
        }
    }

    /// `Arc` user-mapper'а для долгоживущих snapshot'ов (например,
    /// `UserVmContext` в syscall-handler-ах). Для kernel-AS - `None`.
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
