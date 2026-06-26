//! Драйвер ARM Generic Timer: точка входа, фабрика и probe-функция.

use alloc::{boxed::Box, format, string::String, sync::Arc};

use drivers_common::{
    BootServices, DeviceNode, Driver, DriverFactory, DriverRunError, NodeProperty,
    probe::{ProbeError, ProbeResult},
    services::{
        interrupts::{CpuMask, IrqBinding, IrqBound, IrqPriority},
        timer::TimerService,
    },
};
use drivers_common_aarch64::{
    FdtProbeContext,
    gic_interrupt::{GicInterrupt, parse_gic_interrupt},
    require_compatible,
};

use super::{
    service::{ArmGenericTimerHandle, ArmGenericTimerIrqHandler},
    state::ArmGenericTimerState,
};
use crate::register_driver;

const TIMER_IRQ_PRIORITY: IrqPriority = IrqPriority::HIGHEST;

/// Индекс specifier-а virtual timer в `interrupts` ноды `arm,armv8-timer`.
/// Порядок specifier-ов: secure-physical, non-secure-physical, virtual, hypervisor.
const VIRTUAL_TIMER_SPEC_INDEX: usize = 2;

/// Runtime-драйвер ARM Generic Timer.
pub struct ArmGenericTimerDriver {
    interrupt: GicInterrupt,
    irq_bound: Option<IrqBound>,
}

impl ArmGenericTimerDriver {
    pub const fn new(interrupt: GicInterrupt) -> Self {
        Self {
            interrupt,
            irq_bound: None,
        }
    }

    fn map_irq_registration_error(
        err: drivers_common::services::interrupts::IrqRegistrationError,
    ) -> DriverRunError {
        DriverRunError::Fatal(format!("failed to bind generic timer IRQ: {err:?}"))
    }
}

impl Driver for ArmGenericTimerDriver {
    fn run(&mut self, services: &mut BootServices) -> Result<(), DriverRunError> {
        let interrupts = services
            .require_interrupts()
            .map_err(DriverRunError::from_boot_services_error)?;

        let state = ArmGenericTimerState::new().map_err(DriverRunError::Fatal)?;
        let state = Arc::new(state);

        let binding = IrqBinding::new(
            self.interrupt.irq,
            Some(self.interrupt.trigger),
            TIMER_IRQ_PRIORITY,
            CpuMask::CPU0,
            Box::new(ArmGenericTimerIrqHandler::new(state.clone())),
        );

        let irq_bound = interrupts
            .bind(binding)
            .map_err(Self::map_irq_registration_error)?;

        let timer_service: Arc<dyn TimerService> = Arc::new(ArmGenericTimerHandle::new(state));

        services
            .set_timer(timer_service)
            .map_err(DriverRunError::from_boot_services_error)?;

        self.irq_bound = Some(irq_bound);

        Ok(())
    }
}

struct ArmGenericTimerFactory {
    interrupt: GicInterrupt,
}

impl DriverFactory for ArmGenericTimerFactory {
    fn create(&self) -> Result<Box<dyn Driver>, String> {
        Ok(Box::new(ArmGenericTimerDriver::new(self.interrupt)))
    }
}

pub fn arm_generic_timer_probe(context: &mut FdtProbeContext<'_>) -> ProbeResult {
    require_compatible(context.node(), &["arm,armv8-timer", "arm,armv7-timer"])?;

    let interrupts = context
        .node()
        .prop("interrupts")
        .ok_or(ProbeError::MissingProperty("interrupts"))?;

    let interrupt = parse_gic_interrupt(interrupts.raw(), VIRTUAL_TIMER_SPEC_INDEX).ok_or(
        ProbeError::Unsupported("unsupported virtual timer interrupt specifier"),
    )?;

    Ok(Box::new(ArmGenericTimerFactory { interrupt }))
}

register_driver!(ARM_GENERIC_TIMER_DRIVER, probe = arm_generic_timer_probe);
