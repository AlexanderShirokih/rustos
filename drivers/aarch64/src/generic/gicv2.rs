//! Реализация GICv2 (Generic Interrupt Controller версии 2).

use crate::register_driver;
use alloc::boxed::Box;
use alloc::format;
use alloc::string::{String, ToString};
use drivers_common::driver::DriverFactory;
use drivers_common::{
    DeviceMemoryPermission, Driver, DriverContext, MmioAddress, MmioBound, Owners, ProbeResult,
};
use drivers_common_aarch64::{FdtProbeContext, ProbeContextExt, require_compatible};
use interrupts::{
    CpuMask, InterruptController, InterruptControllerInitializationError, IrqNumber, IrqPriority,
    IrqType,
};
use io::mmio::Reg;
use klog::debug;
// ============================================================================
// GICD (Distributor) регистры
// ============================================================================

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

// ============================================================================
// GICC (CPU Interface) регистры
// ============================================================================

/// GICC Control Register — включение CPU interface.
const GICC_CTLR: Reg<u32> = Reg::new(0x000);
/// GICC Priority Mask Register — фильтр приоритетов.
const GICC_PMR: Reg<u32> = Reg::new(0x004);
/// GICC Interrupt Acknowledge Register — чтение номера прерывания.
const GICC_IAR: Reg<u32> = Reg::new(0x00C);
/// GICC End Of Interrupt Register — подтверждение обработки.
const GICC_EOIR: Reg<u32> = Reg::new(0x010);

// ============================================================================
// Константы
// ============================================================================

const SPURIOUS_IRQ_MIN: u32 = 1020;

// ============================================================================
// Драйвер GICv2
// ============================================================================

/// Драйвер Generic Interrupt Controller v2.
pub struct Gicv2 {
    /// MMIO-доступ к GICD (Distributor).
    gicd: MmioBound,

    /// MMIO-доступ к GICC (CPU Interface).
    gicc: MmioBound,
}

// SAFETY: Gicv2 содержит только Mmio (который Sync).
// Все операции с Mmio выполняются через volatile, что безопасно для многопоточности.
unsafe impl Send for Gicv2 {}

impl Gicv2 {
    /// Создаёт новый экземпляр драйвера GICv2.
    ///
    /// # Параметры
    /// - `gicd_base`: физический адрес Distributor.
    /// - `gicc_base`: физический адрес CPU Interface.
    pub const fn new(gicd: MmioBound, gicc: MmioBound) -> Self {
        Self { gicd, gicc }
    }
}

impl Driver for Gicv2 {
    fn run(&mut self) -> Result<(), String> {
        // Инициализация контроллера через трейт InterruptController
        InterruptController::init(self).map_err(|err| err.to_string())
    }
}

impl InterruptController for Gicv2 {
    fn init(&mut self) -> Result<(), InterruptControllerInitializationError> {
        debug!("Initializing GICv2");
        debug!("  GICD @ {}", self.gicd.base());
        debug!("  GICC @ {}", self.gicc.base());

        // 1. Включить Distributor
        self.gicd.write_reg(GICD_CTLR, 1);

        // 2. Включить CPU Interface
        self.gicc.write_reg(GICC_CTLR, 1);

        // 3. Установить маску приоритета (0xFF = разрешить все приоритеты)
        self.gicc.write_reg(GICC_PMR, 0xFF);

        debug!("GICv2 initialized successfully");
        Ok(())
    }

    fn enable(&mut self, irq: IrqNumber) {
        let irq_raw = irq.raw() as usize;
        let reg_index = irq_raw / 32;
        let bit_index = irq_raw % 32;

        // GICD_ISENABLERn[bit] = 1
        let reg = Reg::<u32>::new(GICD_ISENABLER.offset + reg_index * 4);
        self.gicd.write_reg(reg, 1 << bit_index);
    }

    fn disable(&mut self, irq: IrqNumber) {
        let irq_raw = irq.raw() as usize;
        let reg_index = irq_raw / 32;
        let bit_index = irq_raw % 32;

        // GICD_ICENABLERn[bit] = 1
        let reg = Reg::<u32>::new(GICD_ICENABLER.offset + reg_index * 4);
        self.gicd.write_reg(reg, 1 << bit_index);
    }

    fn set_priority(&mut self, irq: IrqNumber, priority: IrqPriority) {
        let irq_raw = irq.raw() as usize;
        let reg_index = irq_raw / 4;
        let byte_offset = irq_raw % 4;

        // GICD_IPRIORITYRn: 4 приоритета по 8 бит на регистр
        let reg_offset = GICD_IPRIORITYR.offset + reg_index * 4;
        let mut val = self.gicd.read::<u32>(reg_offset);

        // Очистить старое значение и установить новое
        let shift = byte_offset * 8;
        val &= !(0xFF << shift);
        val |= (priority.raw() as u32) << shift;

        self.gicd.write::<u32>(reg_offset, val);
    }

    fn set_target(&mut self, irq: IrqNumber, target: CpuMask) {
        let irq_raw = irq.raw() as usize;

        if !matches!(IrqType::from_irq_number(irq), Some(IrqType::Spi(_))) {
            return;
        }

        let reg_index = irq_raw / 4;
        let byte_offset = irq_raw % 4;

        // GICD_ITARGETSRn: 4 маски по 8 бит на регистр
        let reg_offset = GICD_ITARGETSR.offset + reg_index * 4;
        let mut val = self.gicd.read::<u32>(reg_offset);

        // Очистить старое значение и установить новое
        let shift = byte_offset * 8;
        val &= !(0xFF << shift);
        val |= (target.raw() as u32) << shift;

        self.gicd.write::<u32>(reg_offset, val);
    }

    fn acknowledge(&self) -> Option<IrqNumber> {
        let raw = self.gicc.read_reg(GICC_IAR) & 0x3FF;

        if raw >= SPURIOUS_IRQ_MIN {
            // Spurious interrupt (1020-1023)
            None
        } else {
            Some(IrqNumber::new(raw as u16))
        }
    }

    fn end_of_interrupt(&self, irq: IrqNumber) {
        self.gicc.write_reg(GICC_EOIR, irq.raw() as u32);
    }
}

struct Gicv2Factory {
    gicd: MmioAddress,
    gicc: MmioAddress,
}

impl DriverFactory for Gicv2Factory {
    fn create(&self, context: &mut DriverContext) -> Result<Box<dyn Driver>, String> {
        let permissions =
            Owners::<DeviceMemoryPermission>::kernel(DeviceMemoryPermission::writable());

        // Маппинг MMIO-регионов для GICD и GICC
        let gicd = context
            .register_mmio(self.gicd, permissions)
            .map_err(|e| format!("Failed to register GICD. Caused by:{e}"))?;

        let gicc = context
            .register_mmio(self.gicc, permissions)
            .map_err(|e| format!("Failed to register GICC. Caused by:{e}"))?;

        Ok(Box::new(Gicv2::new(gicd, gicc)))
    }
}

/// Probe-функция для GICv2, вызываемая при сканировании Device Tree.
///
/// Извлекает два reg-региона:
/// - reg[0]: GICD (Distributor)
/// - reg[1]: GICC (CPU Interface)
pub fn gicv2_probe(context: &mut FdtProbeContext<'_>) -> ProbeResult {
    require_compatible(context.node(), &["arm,cortex-a15-gic", "arm,gic-400"])?;

    // Чтение адресов из Device Tree
    let gicd = context
        .reg_mmio_address(0)
        .expect("failed to get GICD_BASE");

    let gicc = context
        .reg_mmio_address(1)
        .expect("failed to get GICC_BASE");

    debug!("Probed GICv2: GICD={gicd}, GICC={gicc}");

    Ok(Box::new(Gicv2Factory { gicd, gicc }))
}

// Регистрация драйвера в глобальном реестре
register_driver!(GIC_V2_DRIVER, probe = gicv2_probe);
