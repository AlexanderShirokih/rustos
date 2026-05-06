//! Драйвер GICv3: точка входа, фабрика и probe-функция.

use alloc::{
    boxed::Box,
    string::{String, ToString},
    sync::Arc,
};

use drivers_common::{
    CapabilityStoreExt, CapabilityStoreMut, CapabilityStoreMutExt, DeviceMemoryPermission, Driver,
    DriverFactory, DriverRunError, Owners,
    probe::{ProbeError, ProbeResult},
    services::{
        interrupts::InterruptsService,
        mmio::{MmioAddress, MmioService},
    },
};
use drivers_common_aarch64::{FdtProbeContext, ProbeContextExt, require_compatible};
use spin::Mutex;

use super::{controller::Gicv3Controller, service::GicV3InterruptsService};
use crate::register_driver;

/// Драйвер Generic Interrupt Controller v3.
pub struct Gicv3 {
    /// Адрес MMIO GICD (Distributor).
    distributor_address: MmioAddress,
    /// Адрес MMIO GICR (Redistributor).
    redistributor_address: MmioAddress,
}

// SAFETY: Gicv3 содержит только MmioAddress (Copy-тип без внутреннего состояния).
unsafe impl Send for Gicv3 {}

impl Gicv3 {
    pub fn new(distributor_address: MmioAddress, redistributor_address: MmioAddress) -> Self {
        Self {
            distributor_address,
            redistributor_address,
        }
    }

    fn create_controller(&self, mmio: &dyn MmioService) -> Result<Gicv3Controller, String> {
        let distributor = mmio
            .map_mmio(
                self.distributor_address,
                Owners::<DeviceMemoryPermission>::kernel(DeviceMemoryPermission::writable()),
            )
            .map_err(|err| err.to_string())?;

        let redistributor = mmio
            .map_mmio(
                self.redistributor_address,
                Owners::<DeviceMemoryPermission>::kernel(DeviceMemoryPermission::writable()),
            )
            .map_err(|err| err.to_string())?;

        Ok(Gicv3Controller::new(distributor, redistributor))
    }
}

impl Driver for Gicv3 {
    fn run(&mut self, caps: &mut dyn CapabilityStoreMut) -> Result<(), DriverRunError> {
        let mmio = caps
            .require_service::<dyn MmioService>()
            .map_err(DriverRunError::from_capability_error)?;

        let controller = Arc::new(Mutex::new(
            self.create_controller(mmio.as_ref())
                .map_err(DriverRunError::Fatal)?,
        ));

        {
            controller.lock().init();
        }

        let handle = GicV3InterruptsService::new(controller);

        caps.provide_service::<dyn InterruptsService>(Arc::new(handle))
            .map_err(|err| DriverRunError::Fatal(err.to_string()))
    }
}

struct Gicv3Factory {
    gicd: MmioAddress,
    gicr: MmioAddress,
}

impl DriverFactory for Gicv3Factory {
    fn create(&self) -> Result<Box<dyn Driver>, String> {
        Ok(Box::new(Gicv3::new(self.gicd, self.gicr)))
    }
}

pub fn gicv3_probe(context: &mut FdtProbeContext<'_>) -> ProbeResult {
    require_compatible(context.node(), &["arm,gic-v3"])?;

    let distributor = context
        .get_mmio_address(0)
        .ok_or(ProbeError::MissingProperty("GICD base address"))?;

    let redistributor = context
        .get_mmio_address(1)
        .ok_or(ProbeError::MissingProperty("GICR base address"))?;

    Ok(Box::new(Gicv3Factory {
        gicd: distributor,
        gicr: redistributor,
    }))
}

register_driver!(GIC_V3_DRIVER, probe = gicv3_probe);
