//! Реализация GICv2 (Generic Interrupt Controller версии 2).
//! https://developer.arm.com/documentation/ihi0048/latest/
//!
use crate::register_driver;
use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::string::{String, ToString};
use alloc::sync::Arc;
use drivers_common::probe::ProbeResult;
use drivers_common::services::interrupts::{
    CpuMask, InterruptsService, IrqBinding, IrqBound, IrqHandler, IrqNumber, IrqPriority,
    IrqRegistrationError,
};
use drivers_common::services::mmio::{MmioAddress, MmioBound, MmioService};
use drivers_common::{
    CapabilityStoreExt, CapabilityStoreMut, CapabilityStoreMutExt, DeviceMemoryPermission, Driver,
    DriverFactory, DriverRunError, Owners,
};
use drivers_common_aarch64::{FdtProbeContext, ProbeContextExt, require_compatible};
use io::mmio::Reg;
use klog::debug;
use spin::Mutex;

/// Регистры Distributor interface

/// Control Register — включение distributor.
const GICD_CTLR: Reg<u32> = Reg::new(0x000);
/// Type Register — количество поддерживаемых линий IRQ.
const GICD_TYPER: Reg<u32> = Reg::new(0x004);
/// Interrupt Set-Enable Registers (32 IRQ на регистр).
const GICD_ISENABLER: Reg<u32> = Reg::new(0x100);
/// Interrupt Clear-Enable Registers (32 IRQ на регистр).
const GICD_ICENABLER: Reg<u32> = Reg::new(0x180);
/// Interrupt Clear-Pending Registers (32 IRQ на регистр).
const GICD_ICPENDR: Reg<u32> = Reg::new(0x280);
/// Interrupt Priority Registers (4 IRQ на регистр, по 8 бит на приоритет).
const GICD_IPRIORITYR: Reg<u32> = Reg::new(0x400);
/// Interrupt Processor Targets Registers (4 IRQ на регистр, по 8 бит на маску CPU).
const GICD_ITARGETSR: Reg<u32> = Reg::new(0x800);

/// Interrupt Configuration Registers
const GICD_ICFGR: Reg<u32> = Reg::new(0xC00);

/// Регистры CPU Interface

/// CPU Interface Control Register — включение CPU interface.
const GICC_CTLR: Reg<u32> = Reg::new(0x000);
/// Interrupt Priority Mask Register — фильтр приоритетов.
const GICC_PMR: Reg<u32> = Reg::new(0x004);
/// Binary Point — Определяет разделение между группой приоритета и подприоритетом (0-7)
const GICC_BPR: Reg<u32> = Reg::new(0x008);
/// Interrupt Acknowledge Register — чтение номера прерывания (Group 0, Secure view).
const GICC_IAR: Reg<u32> = Reg::new(0x00C);
/// End of Interrupt Register — подтверждение обработки (Group 0, Secure view).
const GICC_EOIR: Reg<u32> = Reg::new(0x010);

const PRIORITY_MASK_ALL: u32 = 0xFF;

// ITLinesNumber = количество блоков по 32 IRQ каждый.
type ITLinesNumber = usize;

/// Тип прерывания в контексте GIC.
enum IrqType {
    /// Software Generated Interrupt (0–15).
    Sgi,
    /// Private Peripheral Interrupt (16–31).
    Ppi,
    /// Shared Peripheral Interrupt (32–1019).
    Spi,
    /// Spurious interrupt
    Spurious,
}

impl IrqType {
    const fn from_irq_number(irq: IrqNumber) -> Self {
        match irq.raw() {
            0..=15 => IrqType::Sgi,
            16..=31 => IrqType::Ppi,
            32..=1019 => IrqType::Spi,
            _ => IrqType::Spurious,
        }
    }
}

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

/// Runtime-объект контроллера прерываний GICv2, публикуемый через capability.
struct Gicv2Controller {
    distributor: MmioBound,
    cpu_interface: MmioBound,
    interrupt_lines: ITLinesNumber,
    handlers: BTreeMap<IrqNumber, Box<dyn IrqHandler>>,
}

// SAFETY: Gicv2Controller содержит только Mmio (который Sync).
// Все операции с Mmio выполняются через volatile, что безопасно для многопоточности.
unsafe impl Send for Gicv2Controller {}

impl Gicv2Controller {
    pub(crate) fn new(distributor: MmioBound, cpu_interface: MmioBound) -> Self {
        Self {
            handlers: BTreeMap::new(),
            interrupt_lines: Self::get_interrupts_lines(&distributor),
            distributor,
            cpu_interface,
        }
    }

    fn init(&self) {
        Self::set_irq_mask();

        debug!("Enabling GICv2");
        debug!("  GICD @ {}", self.distributor.base());
        debug!("  GICC @ {}", self.cpu_interface.base());

        self.init_distributor();
        self.init_cpu_interface();
    }

    fn enable_global(&self) {
        Self::clear_irq_mask()
    }

    fn disable_global(&self) {
        Self::set_irq_mask();

        // self.cpu_interface.write_reg(GICC_CTLR, 0);
        // self.distributor.write_reg(GICD_CTLR, 0);
    }

    fn init_distributor(&self) {
        // Отключаем Distributor перед настройкой
        self.distributor.write_reg(GICD_CTLR, 0);

        // Сбрасываем прерывания, pending флаги и configuration
        self.reset_interrupt_lines();
        self.set_level_sensitive_mode();

        // Включаем Distributor (EnableGrp0)
        self.distributor.write_reg(GICD_CTLR, 1);
    }

    fn init_cpu_interface(&self) {
        // Устанавливаем маски приоритета
        self.cpu_interface.write_reg(GICC_PMR, PRIORITY_MASK_ALL);
        self.cpu_interface.write_reg(GICC_BPR, 0);

        // Включаем CPU interface (EnableGrp0)
        self.cpu_interface.write_reg(GICC_CTLR, 0x01);
    }

    fn get_interrupts_lines(distributor: &MmioBound) -> ITLinesNumber {
        let typer = distributor.read_reg(GICD_TYPER);
        let it_lines_number = typer & 0x1F;

        (it_lines_number + 1) as usize
    }

    fn reset_interrupt_lines(&self) {
        for bank in 0..self.interrupt_lines {
            let offset = bank * 4;

            // Отключаем все прерывания
            self.distributor
                .write_reg(GICD_ICENABLER.with_offset(offset), u32::MAX);

            // Сбрасываем pending флаги
            self.distributor
                .write_reg(GICD_ICPENDR.with_offset(offset), u32::MAX);
        }
    }

    fn set_level_sensitive_mode(&self) {
        let num_config_regs = self.interrupt_lines * 2;

        for i in 0..num_config_regs {
            self.distributor.write_reg(GICD_ICFGR.with_offset(i * 4), 0);
        }
    }

    fn clear_irq_mask() {
        // SAFETY: Очистка бита I в DAIF разрешает обработку IRQ на текущем CPU.
        unsafe {
            core::arch::asm!("msr daifclr, #0b0010", options(nostack, preserves_flags));
        }
    }

    fn set_irq_mask() {
        // SAFETY: Установка бита I в DAIF запрещает обработку IRQ на текущем CPU.
        unsafe {
            core::arch::asm!("msr daifset, #0b0010", options(nostack, preserves_flags));
        }
    }

    fn enable(&self, irq: IrqNumber) {
        let (reg, bit) = bit_offset(irq, 1);
        self.distributor
            .write_reg(GICD_ISENABLER.with_offset(reg * 4), 1 << bit);
    }

    fn disable(&self, irq: IrqNumber) {
        let (reg, bit) = bit_offset(irq, 1);

        self.distributor
            .write_reg(GICD_ISENABLER.with_offset(reg * 4), 1 << bit);
    }

    fn set_priority(&self, irq: IrqNumber, priority: IrqPriority) {
        let (reg, bit) = bit_offset(irq, 8);

        let priority_reg = GICD_IPRIORITYR.with_offset(reg * 4);
        let mut val = self.distributor.read_reg(priority_reg);

        // Очищаем старое значение и устанавливаем новое
        let bit_shift = bit * 8;
        val &= !(0xFF << bit_shift);
        val |= (priority.raw() as u32) << bit_shift;

        self.distributor.write_reg(priority_reg, val);
    }

    fn set_target_cpu(&self, irq: IrqNumber, target: CpuMask) {
        if !matches!(IrqType::from_irq_number(irq), IrqType::Spi) {
            return;
        }

        let (reg, bit) = bit_offset(irq, 8);

        let target_cpu_reg = GICD_ITARGETSR.with_offset(reg * 4);
        let mut val = self.distributor.read_reg(target_cpu_reg);

        // Очищаем старое значение и устанавливаем новое
        let bit_shift = bit * 8;
        val &= !(0xFF << bit_shift);
        val |= (target.raw() as u32) << bit_shift;

        self.distributor.write_reg(target_cpu_reg, val);
    }

    fn dispatch_interrupt(&mut self) {
        let Some(irq) = self.acknowledge() else {
            return;
        };

        if let Some(handler) = self.handlers.get(&irq) {
            handler.handle();
        }

        self.end_of_interrupt(irq);
    }

    fn acknowledge(&mut self) -> Option<IrqNumber> {
        let raw = (self.cpu_interface.read_reg(GICC_IAR) & 0x3FF) as u16;

        match IrqType::from_irq_number(IrqNumber::new(raw)) {
            IrqType::Spurious => None,
            _ => Some(IrqNumber::new(raw)),
        }
    }

    fn end_of_interrupt(&self, irq: IrqNumber) {
        self.cpu_interface.write_reg(GICC_EOIR, irq.raw() as u32);
    }
}

struct GicInterruptsService {
    controller: Arc<Mutex<Gicv2Controller>>,
}

impl GicInterruptsService {
    fn new(controller: Arc<Mutex<Gicv2Controller>>) -> Self {
        Self { controller }
    }
}

impl InterruptsService for GicInterruptsService {
    fn enable(&self) {
        {
            let guard = self.controller.lock();
            guard.enable_global();
        }
    }

    fn disable(&self) {
        let guard = self.controller.lock();
        guard.disable_global();
    }

    fn bind(&self, binding: IrqBinding) -> Result<IrqBound, IrqRegistrationError> {
        let IrqBinding {
            irq,
            priority,
            target,
            handler,
        } = binding;

        debug!("Binding {irq:?}");

        if matches!(IrqType::from_irq_number(irq), IrqType::Spurious) {
            return Err(IrqRegistrationError::InvalidIrq);
        }

        let mut guard = self.controller.lock();

        if guard.handlers.contains_key(&irq) {
            return Err(IrqRegistrationError::AlreadyRegistered);
        }

        guard.set_priority(irq, priority);
        guard.set_target_cpu(irq, target);

        guard.handlers.insert(irq, handler);
        guard.enable(irq);

        let controller = self.controller.clone();
        let cleanup = move || {
            let mut guard = controller.lock();

            guard.disable(irq);
            guard.handlers.remove(&irq);
        };

        debug!("IRQ {irq:?} bound!");

        Ok(IrqBound::new(irq, cleanup))
    }

    fn dispatch_interrupt(&self) {
        let mut guard = self.controller.lock();
        guard.dispatch_interrupt();
    }
}

#[inline]
const fn bit_offset(irq: IrqNumber, bits: usize) -> (usize, usize) {
    let bank_width = u32::BITS as usize;
    let entries_per_bank = bank_width / bits;
    let raw = irq.raw() as usize;

    (raw / entries_per_bank, raw % entries_per_bank)
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
