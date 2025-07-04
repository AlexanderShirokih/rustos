use crate::kernel::device::registry::DeviceRegistry;
use crate::kernel::memory::memory_map::MemoryMap;
use crate::kernel::memory::physical::PhysicalMemoryManager;

/// Kernel struct that contains core kernel components
pub struct Kernel {
    /// Device registry for managing hardware devices
    device_registry: &'static DeviceRegistry,
    /// Information about boot parameters
    boot_info: BootInfo,
}

pub trait AbstractKernel {
    fn setup_memory(&self, pmm: &mut PhysicalMemoryManager) -> ();
}

pub struct BootInfo {
    pub(crate) memory_map: MemoryMap,
}

impl BootInfo {
    pub(crate) fn new(memory_map: MemoryMap) -> Self {
        Self { memory_map }
    }
    /// Get a reference to the memory map
    pub fn memory_map(&self) -> &MemoryMap {
        &self.memory_map
    }
}

impl Kernel {
    /// Create a new Kernel instance
    pub(crate) fn new(device_registry: &'static DeviceRegistry, boot_info: BootInfo) -> Self {
        Self {
            device_registry,
            boot_info,
        }
    }

    /// Get a reference to the device registry
    pub fn device_registry(&self) -> &DeviceRegistry {
        self.device_registry
    }

    /// Get a reference to the boot info
    pub fn boot_info(&self) -> &BootInfo {
        &self.boot_info
    }
}

impl AbstractKernel for Kernel {
    fn setup_memory(&self, pmm: &mut PhysicalMemoryManager) -> () {
        crate::kernel::arch::setup_memory(self, pmm);
    }
}
