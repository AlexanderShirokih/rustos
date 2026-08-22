//! Драйвер GICv3: точка входа, фабрика и probe-функция.

use alloc::{
    boxed::Box,
    string::{String, ToString},
    sync::Arc,
};

use collections::{LockCell, MutexCell};
use drivers_common::{
    BootServices, DeviceNode, Driver, DriverFactory, DriverRunError, NodeProperty, Owners,
    UncachedPermission,
    probe::{ProbeError, ProbeResult},
    services::mmio::{MmioAddress, MmioService},
};
use drivers_common_aarch64::{FdtProbeContext, ProbeContextExt, require_compatible};

use super::{
    controller::Gicv3Controller,
    regs::{GICR_DEFAULT_STRIDE, GICR_FRAME_SPAN},
    service::GicV3InterruptsService,
};
use crate::register_driver;

/// Драйвер Generic Interrupt Controller v3.
pub struct Gicv3 {
    distributor_address: MmioAddress,
    redistributor_address: MmioAddress,
    redistributor_stride: usize,
}

// SAFETY: Gicv3 содержит только MmioAddress (Copy-тип без внутреннего состояния).
unsafe impl Send for Gicv3 {}

impl Gicv3 {
    pub fn new(
        distributor_address: MmioAddress,
        redistributor_address: MmioAddress,
        redistributor_stride: usize,
    ) -> Self {
        Self {
            distributor_address,
            redistributor_address,
            redistributor_stride,
        }
    }

    fn create_controller(&self, mmio: &dyn MmioService) -> Result<Gicv3Controller, String> {
        let distributor = mmio
            .map_mmio(
                self.distributor_address,
                Owners::<UncachedPermission>::kernel(UncachedPermission::writable()),
            )
            .map_err(|err| err.to_string())?;

        let redistributor = mmio
            .map_mmio(
                self.redistributor_address,
                Owners::<UncachedPermission>::kernel(UncachedPermission::writable()),
            )
            .map_err(|err| err.to_string())?;

        Ok(Gicv3Controller::new(
            distributor,
            redistributor,
            self.redistributor_stride,
        ))
    }
}

impl Driver for Gicv3 {
    fn run(&mut self, services: &mut BootServices) -> Result<(), DriverRunError> {
        let mmio = services
            .require_mmio()
            .map_err(DriverRunError::from_boot_services_error)?;

        let controller = Arc::new(MutexCell::new(
            self.create_controller(mmio.as_ref())
                .map_err(DriverRunError::Fatal)?,
        ));

        {
            controller.with_lock(|controller| controller.init());
        }

        let handle = GicV3InterruptsService::new(controller);

        services
            .set_interrupts(Arc::new(handle))
            .map_err(DriverRunError::from_boot_services_error)
    }
}

struct Gicv3Factory {
    gicd: MmioAddress,
    gicr: MmioAddress,
    gicr_stride: usize,
}

impl DriverFactory for Gicv3Factory {
    fn create(&self) -> Result<Box<dyn Driver>, String> {
        Ok(Box::new(Gicv3::new(self.gicd, self.gicr, self.gicr_stride)))
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

    // Отображается только первая объявленная область Redistributor. CPU,
    // чей фрейм лежит в остальных, не найдёт себя и в работу не пойдёт -
    // отказ виден на его старте, а не молча.
    let redistributor_stride = context
        .node()
        .prop("redistributor-stride")
        .and_then(|prop| read_stride(prop.raw()))
        .unwrap_or(GICR_DEFAULT_STRIDE);

    Ok(Box::new(Gicv3Factory {
        gicd: distributor,
        gicr: redistributor,
        gicr_stride: redistributor_stride,
    }))
}

fn read_stride(raw: &[u8]) -> Option<usize> {
    let bytes: [u8; 8] = raw.first_chunk().copied()?;
    let stride = usize::try_from(u64::from_be_bytes(bytes)).ok()?;

    (stride >= GICR_FRAME_SPAN).then_some(stride)
}

register_driver!(GIC_V3_DRIVER, probe = gicv3_probe);
