use alloc::sync::Arc;

use drivers_common::{
    Capabilities, CapabilityStoreMutExt, RuntimeDriverRegistry, services::mmio::MmioService,
};
use memory::{
    memory_mapper::{AddressSpaceFactory, MemoryMapper},
    virtual_address::PageAlignedVirtualAddress,
};
use spin::Mutex;

use crate::{services::mmio::MmioServiceImpl, syscall_bridge};

pub struct KernelContext {
    capabilities: Capabilities,
    driver_registry: Mutex<RuntimeDriverRegistry>,
    address_space_factory: &'static (dyn AddressSpaceFactory + Send + Sync),
}

impl KernelContext {
    pub fn new(
        memory_mapper: &'static (dyn MemoryMapper + Send + Sync),
        address_space_factory: &'static (dyn AddressSpaceFactory + Send + Sync),
        base_offset: PageAlignedVirtualAddress,
    ) -> KernelContext {
        // Дублируем фабрику в syscall_bridge для модулей, у которых нет
        // прямого доступа к KernelContext.
        syscall_bridge::install_address_space_factory(address_space_factory);

        let mmio_service: Arc<dyn MmioService> = Arc::new(MmioServiceImpl {
            memory_mapper,
            linear_offset: base_offset,
        });

        let mut capabilities = Capabilities::new();

        capabilities
            .provide_service::<dyn MmioService>(mmio_service)
            .expect("Failed to register MmioService service");

        Self {
            capabilities,
            driver_registry: Mutex::new(RuntimeDriverRegistry::new()),
            address_space_factory,
        }
    }

    /// Фабрика user-AS, переданная в `new`. Используется scheduler-ом для
    /// создания нового адресного пространства при `spawn_user_process`.
    pub fn address_space_factory(&self) -> &'static (dyn AddressSpaceFactory + Send + Sync) {
        self.address_space_factory
    }

    pub fn with_runtime_state<R>(
        &mut self,
        map: impl FnOnce(&mut Capabilities, &mut RuntimeDriverRegistry) -> R,
    ) -> R {
        map(&mut self.capabilities, self.driver_registry.get_mut())
    }
}
