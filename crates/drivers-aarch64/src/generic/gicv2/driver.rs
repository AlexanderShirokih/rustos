//! Драйвер GICv2: точка входа, фабрика и probe-функция.

use alloc::{
    boxed::Box,
    string::{String, ToString},
    sync::Arc,
};

use drivers_common::{
    CapabilityStoreExt, CapabilityStoreMut, CapabilityStoreMutExt, DeviceMemoryPermission, Driver,
    DriverFactory, DriverRunError, Owners,
    probe::ProbeResult,
    services::{
        interrupts::InterruptsService,
        mmio::{MmioAddress, MmioService},
    },
};
use drivers_common_aarch64::{FdtProbeContext, ProbeContextExt, require_compatible};
use spin::Mutex;

use super::{controller::Gicv2Controller, service::GicInterruptsService};
use crate::register_driver;

/// Драйвер Generic Interrupt Controller v2.
pub struct Gicv2 {
    /// Адрес MMIO GICD (Distributor).
    distributor_address: MmioAddress,

    /// Адрес MMIO GICC (CPU Interface).
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
                Owners::<DeviceMemoryPermission>::kernel(DeviceMemoryPermission::writable()),
            )
            .map_err(|err| err.to_string())?;

        let cpu_interface = mmio
            .map_mmio(
                self.cpu_interface_address,
                Owners::<DeviceMemoryPermission>::kernel(DeviceMemoryPermission::writable()),
            )
            .map_err(|err| err.to_string())?;

        Ok(Gicv2Controller::new(distributor, cpu_interface))
    }
}

impl Driver for Gicv2 {
    fn run(&mut self, caps: &mut dyn CapabilityStoreMut) -> Result<(), DriverRunError> {
        let mmio = caps
            .require_service::<dyn MmioService>()
            .map_err(DriverRunError::from_capability_error)?;

        let controller = Arc::new(Mutex::new(
            self.create_controller(mmio.as_ref())
                .map_err(DriverRunError::Fatal)?,
        ));

        {
            controller.lock().init()
        }

        let handle = GicInterruptsService::new(controller);

        caps.provide_service::<dyn InterruptsService>(Arc::new(handle))
            .map_err(|err| DriverRunError::Fatal(err.to_string()))
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

    let gicd = context
        .get_mmio_address(0)
        .expect("failed to get GICD_BASE");

    let gicc = context
        .get_mmio_address(1)
        .expect("failed to get GICC_BASE");

    Ok(Box::new(Gicv2Factory { gicd, gicc }))
}

register_driver!(GIC_V2_DRIVER, probe = gicv2_probe);
