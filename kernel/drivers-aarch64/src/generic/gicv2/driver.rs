//! Драйвер GICv2: точка входа, фабрика и probe-функция.

use alloc::{
    boxed::Box,
    string::{String, ToString},
    sync::Arc,
};

use collections::{LockCell, MutexCell};
use drivers_common::{
    BootServices, Driver, DriverFactory, DriverRunError, Owners, UncachedPermission,
    probe::{ProbeError, ProbeResult},
    services::mmio::{MmioAddress, MmioService},
};
use drivers_common_aarch64::{FdtProbeContext, ProbeContextExt, require_compatible};

use super::{controller::Gicv2Controller, service::GicInterruptsService};
use crate::register_driver;

/// Драйвер Generic Interrupt Controller v2.
pub struct Gicv2 {
    distributor_address: MmioAddress,
    cpu_interface_address: MmioAddress,
}

// SAFETY: Gicv2 содержит только Mmio (который Sync).
// Все операции с Mmio выполняются через volatile
unsafe impl Send for Gicv2 {}

impl Gicv2 {
    pub fn new(distributor_address: MmioAddress, cpu_interface_address: MmioAddress) -> Self {
        Self {
            distributor_address,
            cpu_interface_address,
        }
    }

    fn create_controller(&self, mmio: &dyn MmioService) -> Result<Gicv2Controller, String> {
        let distributor = mmio
            .map_mmio(
                self.distributor_address,
                Owners::<UncachedPermission>::kernel(UncachedPermission::writable()),
            )
            .map_err(|err| err.to_string())?;

        let cpu_interface = mmio
            .map_mmio(
                self.cpu_interface_address,
                Owners::<UncachedPermission>::kernel(UncachedPermission::writable()),
            )
            .map_err(|err| err.to_string())?;

        Ok(Gicv2Controller::new(distributor, cpu_interface))
    }
}

impl Driver for Gicv2 {
    fn run(&mut self, services: &mut BootServices) -> Result<(), DriverRunError> {
        let mmio = services
            .require_mmio()
            .map_err(DriverRunError::from_boot_services_error)?;

        let controller = Arc::new(MutexCell::new(
            self.create_controller(mmio.as_ref())
                .map_err(DriverRunError::Fatal)?,
        ));

        controller
            .with_lock(Gicv2Controller::init)
            .map_err(|e| DriverRunError::Fatal(alloc::format!("GICv2 init failed: {e}")))?;

        let handle = GicInterruptsService::new(controller);

        services
            .set_interrupts(Arc::new(handle))
            .map_err(DriverRunError::from_boot_services_error)
    }
}

struct Gicv2Factory {
    gicd: MmioAddress,
    gicc: MmioAddress,
}

impl DriverFactory for Gicv2Factory {
    fn create(&self) -> Result<Box<dyn Driver>, String> {
        Ok(Box::new(Gicv2::new(self.gicd, self.gicc)))
    }
}

pub fn gicv2_probe(context: &mut FdtProbeContext<'_>) -> ProbeResult {
    require_compatible(context.node(), &["arm,cortex-a15-gic", "arm,gic-400"])?;

    let distributor = context
        .get_mmio_address(0)
        .ok_or(ProbeError::MissingProperty("reg[0] (GICD base)"))?;

    let cpu_interface = context
        .get_mmio_address(1)
        .ok_or(ProbeError::MissingProperty("reg[1] (GICC base)"))?;

    Ok(Box::new(Gicv2Factory {
        gicd: distributor,
        gicc: cpu_interface,
    }))
}

register_driver!(GIC_V2_DRIVER, probe = gicv2_probe);
