use alloc::boxed::Box;
use drivers_common::RuntimeDriverRegistry;
use interrupts::InterruptController;
use memory::memory_mapper::MemoryMapper;
use spin::Mutex;

pub struct KernelContext {
    memory_mapper: &'static dyn MemoryMapper,
    _interrupts: Mutex<Option<&'static dyn InterruptController>>,
    driver_registry: Mutex<RuntimeDriverRegistry>,
}

impl KernelContext {
    pub fn new(memory_mapper: Box<dyn MemoryMapper>) -> KernelContext {
        Self {
            driver_registry: Mutex::new(RuntimeDriverRegistry::new()),
            _interrupts: Mutex::new(None),
            memory_mapper: Box::leak(memory_mapper),
        }
    }

    pub fn memory_mapper(&self) -> &'static dyn MemoryMapper {
        self.memory_mapper
    }

    pub fn driver_registry_mut(&mut self) -> &mut RuntimeDriverRegistry {
        self.driver_registry.get_mut()
    }
}

// impl DriverRegistryBridge for KernelContext {
//     fn memory_mapper(&self) -> &'static dyn MemoryMapper {
//         self.memory_mapper
//     }
//
//     fn interrupt_controller(&self) -> Option<&'static dyn InterruptController> {
//         *self.interrupts.lock()
//     }
// }
