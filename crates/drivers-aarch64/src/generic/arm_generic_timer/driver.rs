//! Драйвер ARM Generic Timer: точка входа, фабрика и probe-функция.

use alloc::{
    boxed::Box,
    format,
    string::{String, ToString},
    sync::Arc,
};
use core::mem::size_of;

use drivers_common::{
    CapabilityStoreExt, CapabilityStoreMut, CapabilityStoreMutExt, DeviceNode, Driver,
    DriverFactory, DriverRunError, NodeProperty,
    probe::{ProbeError, ProbeResult},
    services::{
        interrupts::{CpuMask, InterruptsService, IrqBinding, IrqBound, IrqNumber, IrqPriority},
        timer::TimerService,
    },
};
use drivers_common_aarch64::{FdtProbeContext, require_compatible};

use super::{
    service::{ArmGenericTimerHandle, ArmGenericTimerIrqHandler},
    state::ArmGenericTimerState,
};
use crate::register_driver;

const TIMER_IRQ_PRIORITY: IrqPriority = IrqPriority::HIGHEST;
const GIC_TYPE_SPI: u32 = 0;
const GIC_TYPE_PPI: u32 = 1;
const INTERRUPT_SPECIFIER_CELLS: usize = 3;
const INTERRUPT_CELL_SIZE: usize = size_of::<u32>();
const NONSECURE_PHYSICAL_TIMER_SPEC_INDEX: usize = 1;

/// Runtime-драйвер ARM Generic Timer.
pub struct ArmGenericTimerDriver {
    irq: IrqNumber,
    irq_bound: Option<IrqBound>,
}

impl ArmGenericTimerDriver {
    pub const fn new(irq: IrqNumber) -> Self {
        Self {
            irq,
            irq_bound: None,
        }
    }

    fn map_irq_registration_error(
        err: drivers_common::services::interrupts::IrqRegistrationError,
    ) -> DriverRunError {
        DriverRunError::Fatal(format!("failed to bind generic timer IRQ: {:?}", err))
    }
}

impl Driver for ArmGenericTimerDriver {
    fn run(&mut self, caps: &mut dyn CapabilityStoreMut) -> Result<(), DriverRunError> {
        let interrupts = caps
            .require_service::<dyn InterruptsService>()
            .map_err(DriverRunError::from_capability_error)?;

        let state = ArmGenericTimerState::new().map_err(DriverRunError::Fatal)?;
        let state = Arc::new(state);

        let binding = IrqBinding::new(
            self.irq,
            TIMER_IRQ_PRIORITY,
            CpuMask::CPU0,
            Box::new(ArmGenericTimerIrqHandler::new(state.clone())),
        );

        let irq_bound = interrupts
            .bind(binding)
            .map_err(Self::map_irq_registration_error)?;

        let timer_service: Arc<dyn TimerService> = Arc::new(ArmGenericTimerHandle::new(state));

        caps.provide_service::<dyn TimerService>(timer_service)
            .map_err(|err| DriverRunError::Fatal(err.to_string()))?;

        self.irq_bound = Some(irq_bound);

        Ok(())
    }
}

struct ArmGenericTimerFactory {
    irq: IrqNumber,
}

impl DriverFactory for ArmGenericTimerFactory {
    fn create(&self) -> Result<Box<dyn Driver>, String> {
        Ok(Box::new(ArmGenericTimerDriver::new(self.irq)))
    }
}

pub fn arm_generic_timer_probe(context: &mut FdtProbeContext<'_>) -> ProbeResult {
    require_compatible(context.node(), &["arm,armv8-timer", "arm,armv7-timer"])?;

    let interrupts = context
        .node()
        .prop("interrupts")
        .ok_or(ProbeError::MissingProperty("interrupts"))?;

    let irq = parse_nonsecure_physical_timer_irq(interrupts.raw())?;

    Ok(Box::new(ArmGenericTimerFactory { irq }))
}

fn parse_nonsecure_physical_timer_irq(raw: &[u8]) -> Result<IrqNumber, ProbeError> {
    let spec_size = INTERRUPT_SPECIFIER_CELLS * INTERRUPT_CELL_SIZE;
    if !raw.len().is_multiple_of(spec_size) {
        return Err(ProbeError::Unsupported(
            "invalid interrupts property length for generic timer",
        ));
    }

    let spec_count = raw.len() / spec_size;
    if spec_count <= NONSECURE_PHYSICAL_TIMER_SPEC_INDEX {
        return Err(ProbeError::Unsupported(
            "non-secure physical timer interrupt specifier is missing",
        ));
    }

    let base = NONSECURE_PHYSICAL_TIMER_SPEC_INDEX * spec_size;
    let interrupt_type = read_be_u32(raw, base).ok_or(ProbeError::Unsupported(
        "failed to parse non-secure physical timer interrupt type",
    ))?;
    let interrupt_number = read_be_u32(raw, base + INTERRUPT_CELL_SIZE).ok_or(
        ProbeError::Unsupported("failed to parse non-secure physical timer interrupt number"),
    )?;

    gic_specifier_to_irq(interrupt_type, interrupt_number).ok_or(ProbeError::Unsupported(
        "unsupported non-secure physical timer interrupt specifier",
    ))
}

fn read_be_u32(raw: &[u8], offset: usize) -> Option<u32> {
    let bytes = raw.get(offset..offset + INTERRUPT_CELL_SIZE)?;
    let array: [u8; INTERRUPT_CELL_SIZE] = bytes.try_into().ok()?;
    Some(u32::from_be_bytes(array))
}

fn gic_specifier_to_irq(interrupt_type: u32, interrupt_number: u32) -> Option<IrqNumber> {
    let irq_raw = match interrupt_type {
        GIC_TYPE_SPI => interrupt_number.checked_add(32)?,
        GIC_TYPE_PPI => interrupt_number.checked_add(16)?,
        _ => return None,
    };

    if irq_raw > u16::MAX as u32 {
        return None;
    }

    Some(IrqNumber::new(irq_raw as u16))
}

register_driver!(ARM_GENERIC_TIMER_DRIVER, probe = arm_generic_timer_probe);
