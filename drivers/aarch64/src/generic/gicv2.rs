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

/// GICD Control Register — включение distributor.
const GICD_CTLR: Reg<u32> = Reg::new(0x000);
/// GICD Interrupt Set-Enable Registers (32 IRQ на регистр).
const GICD_ISENABLER: Reg<u32> = Reg::new(0x100);
/// GICD Interrupt Clear-Enable Registers (32 IRQ на регистр).
const GICD_ICENABLER: Reg<u32> = Reg::new(0x180);
/// GICD Interrupt Priority Registers (4 IRQ на регистр, по 8 бит на приоритет).
const GICD_IPRIORITYR: Reg<u32> = Reg::new(0x400);
/// GICD Interrupt Processor Targets Registers (4 IRQ на регистр, по 8 бит на маску CPU).
const GICD_ITARGETSR: Reg<u32> = Reg::new(0x800);

/// GICC Control Register — включение CPU interface.
const GICC_CTLR: Reg<u32> = Reg::new(0x000);
/// GICC Priority Mask Register — фильтр приоритетов.
const GICC_PMR: Reg<u32> = Reg::new(0x004);
/// GICC Interrupt Acknowledge Register — чтение номера прерывания.
const GICC_IAR: Reg<u32> = Reg::new(0x00C);
/// GICC End Of Interrupt Register — подтверждение обработки.
const GICC_EOIR: Reg<u32> = Reg::new(0x010);

const SPURIOUS_IRQ_MIN: u32 = 1020;
const TAG: &str = "GICv2";

fn enable_local_irq() {
    // SAFETY: Очистка бита I в DAIF разрешает обработку IRQ на текущем CPU.
    // Вызывается только после инициализации контроллера прерываний.
    unsafe {
        core::arch::asm!("msr daifclr, #0b0010", options(nostack, preserves_flags));
    }
}

fn disable_local_irq() {
    // SAFETY: Установка бита I в DAIF запрещает обработку IRQ на текущем CPU.
    unsafe {
        core::arch::asm!("msr daifset, #0b0010", options(nostack, preserves_flags));
    }
}

struct IrqIndex {
    pub reg_index: usize,
    pub bit_index: u8,
}

impl IrqIndex {
    fn new(irq: IrqNumber) -> Self {
        let irq_raw = irq.raw();
        let reg_index = irq_raw / 4;
        let byte_offset = irq_raw % 4;

        Self {
            reg_index: reg_index as usize,
            bit_index: byte_offset as u8,
        }
    }
}

/// Тип прерывания в контексте GIC.
enum IrqType {
    /// Software Generated Interrupt (0–15).
    Sgi,
    /// Private Peripheral Interrupt (16–31).
    Ppi,
    /// Shared Peripheral Interrupt (32–1019).
    Spi,
}

impl IrqType {
    /// Создаёт тип прерывания из номера.
    const fn from_irq_number(irq: IrqNumber) -> Option<Self> {
        match irq.raw() {
            0..=15 => Some(IrqType::Sgi),
            16..=31 => Some(IrqType::Ppi),
            32..=1019 => Some(IrqType::Spi),
            _ => None,
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
// Все операции с Mmio выполняются через volatile, что безопасно для многопоточности.
unsafe impl Send for Gicv2 {}

impl Gicv2 {
    pub fn new(distributor_address: MmioAddress, cpu_interface_address: MmioAddress) -> Self {
        Self {
            distributor_address,
            cpu_interface_address,
        }
    }

    fn build_controller(&self, mmio: &dyn MmioService) -> Result<Gicv2Controller, String> {
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

        Ok(Gicv2Controller {
            distributor,
            cpu_interface,
            handlers: BTreeMap::new(),
        })
    }
}

impl Driver for Gicv2 {
    fn run(&mut self, caps: &mut dyn CapabilityStoreMut) -> Result<(), DriverRunError> {
        let mmio = caps
            .require_service::<dyn MmioService>()
            .map_err(DriverRunError::from_capability_error)?;

        let controller = Arc::new(Mutex::new(
            self.build_controller(mmio.as_ref())
                .map_err(DriverRunError::Fatal)?,
        ));

        let handle = GicInterruptsHandle::new(controller);

        caps.provide_service::<dyn InterruptsService>(Arc::new(handle))
            .map_err(|err| DriverRunError::Fatal(err.to_string()))
    }
}

/// Runtime-объект контроллера прерываний GICv2, публикуемый через capability.
struct Gicv2Controller {
    distributor: MmioBound,
    cpu_interface: MmioBound,
    handlers: BTreeMap<IrqNumber, Box<dyn IrqHandler>>,
}

// SAFETY: Gicv2Controller содержит только Mmio (который Sync).
// Все операции с Mmio выполняются через volatile, что безопасно для многопоточности.
unsafe impl Send for Gicv2Controller {}

impl Gicv2Controller {
    fn enable_irq(&mut self, irq: IrqNumber) {
        let IrqIndex {
            reg_index,
            bit_index,
        } = IrqIndex::new(irq);

        // GICD_ISENABLERn[bit] = 1
        let reg = GICD_ISENABLER.with_offset(reg_index * 4);
        self.distributor.write_reg(reg, 1 << bit_index);
    }

    fn disable_irq(&mut self, irq: IrqNumber) {
        let IrqIndex {
            reg_index,
            bit_index,
        } = IrqIndex::new(irq);

        // GICD_ICENABLERn[bit] = 1
        let reg = GICD_ICENABLER.with_offset(reg_index * 4);
        self.distributor.write_reg(reg, 1 << bit_index);
    }

    fn set_priority(&mut self, irq: IrqNumber, priority: IrqPriority) {
        let IrqIndex {
            reg_index,
            bit_index,
        } = IrqIndex::new(irq);

        // GICD_IPRIORITYRn: 4 приоритета по 8 бит на регистр
        let reg_offset = GICD_IPRIORITYR.offset + reg_index * 4;
        let mut val = self.distributor.read::<u32>(reg_offset);

        // Очистить старое значение и установить новое
        let shift = bit_index * 8;
        val &= !(0xFF << shift);
        val |= (priority.raw() as u32) << shift;

        self.distributor.write::<u32>(reg_offset, val);
    }

    fn set_target(&mut self, irq: IrqNumber, target: CpuMask) {
        if !matches!(IrqType::from_irq_number(irq), Some(IrqType::Spi)) {
            return;
        }

        let IrqIndex {
            reg_index,
            bit_index,
        } = IrqIndex::new(irq);

        // GICD_ITARGETSRn: 4 маски по 8 бит на регистр
        let reg_offset = GICD_ITARGETSR.offset + reg_index * 4;
        let mut val = self.distributor.read::<u32>(reg_offset);

        // Очистить старое значение и установить новое
        let shift = bit_index * 8;
        val &= !(0xFF << shift);
        val |= (target.raw() as u32) << shift;

        self.distributor.write::<u32>(reg_offset, val);
    }

    fn acknowledge(&self) -> Option<IrqNumber> {
        let raw = self.cpu_interface.read_reg(GICC_IAR) & 0x3FF;

        if raw >= SPURIOUS_IRQ_MIN {
            // Spurious interrupt (1020-1023)
            None
        } else {
            Some(IrqNumber::new(raw as u16))
        }
    }

    fn end_of_interrupt(&self, irq: IrqNumber) {
        self.cpu_interface.write_reg(GICC_EOIR, irq.raw() as u32);
    }

    fn unbind_irq(&mut self, irq: IrqNumber) {
        self.disable_irq(irq);
        self.handlers.remove(&irq);
    }

    fn enable_global(&self) {
        debug!(TAG;"Enabling GICv2");
        debug!(TAG;"  GICD @ {}", self.distributor.base());
        debug!(TAG;"  GICC @ {}", self.cpu_interface.base());

        // 1. Включить Distributor
        self.distributor.write_reg(GICD_CTLR, 1);

        // 2. Включить CPU Interface
        self.cpu_interface.write_reg(GICC_CTLR, 1);

        // 3. Установить маску приоритета (0xFF = разрешить все приоритеты)
        self.cpu_interface.write_reg(GICC_PMR, 0xFF);
    }

    fn disable_global(&self) {
        self.cpu_interface.write_reg(GICC_CTLR, 0);
        self.distributor.write_reg(GICD_CTLR, 0);
    }

    fn dispatch_interrupt(&self) {
        let Some(irq) = self.acknowledge() else {
            return;
        };

        if let Some(handler) = self.handlers.get(&irq) {
            handler.handle();
        }

        self.end_of_interrupt(irq);
    }
}

struct GicInterruptsHandle {
    controller: Arc<Mutex<Gicv2Controller>>,
}

impl GicInterruptsHandle {
    fn new(controller: Arc<Mutex<Gicv2Controller>>) -> Self {
        Self { controller }
    }
}

impl InterruptsService for GicInterruptsHandle {
    fn enable(&self) {
        {
            let guard = self.controller.lock();
            guard.enable_global();
        }

        enable_local_irq();
    }

    fn disable(&self) {
        disable_local_irq();

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

        let mut guard = self.controller.lock();

        if IrqType::from_irq_number(irq).is_none() {
            return Err(IrqRegistrationError::InvalidIrq);
        }

        if guard.handlers.contains_key(&irq) {
            return Err(IrqRegistrationError::AlreadyRegistered);
        }

        guard.set_priority(irq, priority);
        guard.set_target(irq, target);
        guard.handlers.insert(irq, handler);
        guard.enable_irq(irq);

        let controller = self.controller.clone();
        let cleanup = move || {
            let mut guard = controller.lock();
            guard.unbind_irq(irq);
        };

        Ok(IrqBound::new(irq, cleanup))
    }

    fn dispatch_interrupt(&self) {
        let guard = self.controller.lock();
        guard.dispatch_interrupt();
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

    debug!(TAG; "Probed GICv2: GICD={gicd}, GICC={gicc}");

    Ok(Box::new(Gicv2Factory { gicd, gicc }))
}

register_driver!(GIC_V2_DRIVER, probe = gicv2_probe);
