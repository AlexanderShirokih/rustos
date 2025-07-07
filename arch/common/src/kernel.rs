use kernel_core::device::registry::DeviceRegistry;
use kernel_core::log::OutputStreamLogger;

/// Kernel struct that contains core kernel components
pub struct Kernel {
    /// Device registry for managing hardware devices
    device_registry: &'static DeviceRegistry,

    // Kernel's global logger
    logger: OutputStreamLogger,

    /// Information about boot parameters
    boot_info: BootInfo,
}

impl Kernel {
    /// Create a new Kernel instance
    pub fn new(
        device_registry: &'static DeviceRegistry,
        logger: OutputStreamLogger,
        boot_info: BootInfo,
    ) -> Self {
        Self {
            device_registry,
            logger,
            boot_info,
        }
    }

    pub fn logger(&self) -> & OutputStreamLogger {
        &self.logger
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

pub struct BootInfo {}

impl BootInfo {
    pub fn new() -> Self {
        Self {}
    }
}
