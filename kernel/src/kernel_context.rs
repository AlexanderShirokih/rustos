use crate::services::mmio::MmioServiceImpl;
use alloc::sync::Arc;
use drivers_common::services::mmio::MmioService;
use drivers_common::{Capabilities, CapabilityStoreMutExt, RuntimeDriverRegistry};
use memory::memory_mapper::MemoryMapper;
use memory::virtual_address::PageAlignedVirtualAddress;
use spin::Mutex;

pub struct KernelContext {
    capabilities: Capabilities,
    driver_registry: Mutex<RuntimeDriverRegistry>,
}

impl KernelContext {
    pub fn new(
        memory_mapper: &'static dyn MemoryMapper,
        base_offset: PageAlignedVirtualAddress,
    ) -> KernelContext {
        let mmio_service: Arc<dyn MmioService> = Arc::new(MmioServiceImpl {
            memory_mapper,
            linear_offset: base_offset,
        });

        let mut capabilities = Capabilities::new();

        capabilities
            .provide_service::<dyn MmioService>(mmio_service)
            .expect("Failed to register MmioService service");

        Self {
            driver_registry: Mutex::new(RuntimeDriverRegistry::new()),
            capabilities,
        }
    }

    pub fn with_runtime_state<R>(
        &mut self,
        map: impl FnOnce(&mut Capabilities, &mut RuntimeDriverRegistry) -> R,
    ) -> R {
        map(&mut self.capabilities, self.driver_registry.get_mut())
    }
}
