//! Драйвер ARM Generic Timer.
//! https://developer.arm.com/documentation/100403/latest/

use crate::register_driver;
use alloc::boxed::Box;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::sync::Arc;
use core::mem::size_of;
use core::sync::atomic::{AtomicU32, Ordering};
use drivers_common::probe::{ProbeError, ProbeResult};
use drivers_common::services::interrupts::{
    CpuMask, InterruptsService, IrqBinding, IrqBound, IrqHandler, IrqNumber, IrqPriority,
    IrqRegistrationError,
};
use drivers_common::services::timer::TimerService;
use drivers_common::{
    CapabilityStoreExt, CapabilityStoreMut, CapabilityStoreMutExt, DeviceNode, Driver,
    DriverFactory, DriverRunError, NodeProperty,
};
use drivers_common_aarch64::{FdtProbeContext, require_compatible};
use klog::debug;

const TIMER_IRQ_PRIORITY: IrqPriority = IrqPriority::HIGHEST;
const CNTP_CTL_ENABLE: u32 = 1 << 0;
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

    fn map_irq_registration_error(err: IrqRegistrationError) -> DriverRunError {
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

/// Runtime-состояние ARM Generic Timer.
struct ArmGenericTimerState {
    frequency: u64,
    reload_value: AtomicU32,
}

impl ArmGenericTimerState {
    fn new() -> Result<Self, String> {
        let frequency = Self::read_cntfrq_el0();

        if frequency == 0 {
            return Err("CNTFRQ_EL0 returned zero frequency".into());
        }

        Ok(Self {
            frequency,
            reload_value: AtomicU32::new(0),
        })
    }

    fn set_periodic(&self, interval_ms: u64) {
        let reload_value = Self::compute_counter_value(self.frequency, interval_ms).unwrap_or(1);

        self.reload_value.store(reload_value, Ordering::Relaxed);

        Self::write_cntp_tval_el0(reload_value);
        Self::write_cntp_ctl_el0(CNTP_CTL_ENABLE);
    }

    fn get_elapsed_ns(&self) -> u64 {
        Self::ticks_to_ns(Self::read_cntpct_el0(), self.frequency)
    }

    fn ticks_to_ns(ticks: u64, frequency: u64) -> u64 {
        (ticks as u128 * 1_000_000_000 / frequency as u128) as u64
    }

    fn on_interrupt(&self) {
        debug!("timer tick...");

        let reload_value = self.reload_value.load(Ordering::Relaxed);
        if reload_value != 0 {
            Self::write_cntp_tval_el0(reload_value);
        }
    }

    fn compute_counter_value(frequency: u64, interval_ms: u64) -> Option<u32> {
        let ticks = frequency.checked_mul(interval_ms)? / 1_000;

        if ticks == 0 {
            return None;
        }

        ticks.try_into().ok()
    }

    fn read_cntfrq_el0() -> u64 {
        let value: u64;
        // SAFETY: Чтение системного регистра CNTFRQ_EL0 разрешено на EL1 при корректной
        // конфигурации платформы и не нарушает инварианты памяти.
        unsafe {
            core::arch::asm!("mrs {value}, cntfrq_el0", value = out(reg) value, options(nomem, nostack));
        }
        value
    }

    fn read_cntpct_el0() -> u64 {
        let value: u64;
        // SAFETY: Чтение CNTPCT_EL0 является побочным только по времени и не модифицирует
        // память/состояние, влияющее на безопасность Rust-кода.
        unsafe {
            core::arch::asm!("mrs {value}, cntpct_el0", value = out(reg) value, options(nomem, nostack));
        }
        value
    }

    fn write_cntp_tval_el0(value: u32) {
        let value = value as u64;
        // SAFETY: Запись в CNTP_TVAL_EL0 программирует относительный дедлайн физического таймера.
        // Аппаратно устанавливает CNTP_CVAL_EL0 = CNTPCT_EL0 + TVAL.
        unsafe {
            core::arch::asm!(
            "msr cntp_tval_el0, {value}",
            "isb",
            value = in(reg) value,
            options(nomem, nostack),
            );
        }
    }

    fn write_cntp_ctl_el0(value: u32) {
        let value = value as u64;
        // SAFETY: Запись в CNTP_CTL_EL0 меняет только биты управления физического таймера.
        // Используются только документированные значения (enable/unmask).
        unsafe {
            core::arch::asm!(
            "msr cntp_ctl_el0, {value}",
            "isb",
            value = in(reg) value,
            options(nomem, nostack),
            );
        }
    }
}

struct ArmGenericTimerIrqHandler {
    state: Arc<ArmGenericTimerState>,
}

impl ArmGenericTimerIrqHandler {
    fn new(state: Arc<ArmGenericTimerState>) -> Self {
        Self { state }
    }
}

impl IrqHandler for ArmGenericTimerIrqHandler {
    fn handle(&self) {
        self.state.on_interrupt();
    }
}

struct ArmGenericTimerHandle {
    state: Arc<ArmGenericTimerState>,
}

impl ArmGenericTimerHandle {
    fn new(state: Arc<ArmGenericTimerState>) -> Self {
        Self { state }
    }
}

impl TimerService for ArmGenericTimerHandle {
    fn time_monotonic_elapsed(&self) -> u64 {
        self.state.get_elapsed_ns()
    }

    fn set_periodic(&self, interval_ms: u64) {
        self.state.set_periodic(interval_ms);
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
