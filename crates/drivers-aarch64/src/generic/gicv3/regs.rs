//! Определения MMIO-регистров GICv3 и вспомогательные типы.

use drivers_common::services::interrupts::IrqNumber;
use io::mmio::Reg;

/// Регистры Distributor (GICD)

/// Control Register - включение дистрибьютора (EnableGrp1NS, ARE_NS).
pub(super) const GICD_CTLR: Reg<u32> = Reg::new(0x000);
/// Type Register - количество поддерживаемых линий IRQ.
pub(super) const GICD_TYPER: Reg<u32> = Reg::new(0x004);
/// Interrupt Group Registers (32 IRQ на регистр).
pub(super) const GICD_IGROUPR: Reg<u32> = Reg::new(0x080);
/// Interrupt Set-Enable Registers (32 IRQ на регистр).
pub(super) const GICD_ISENABLER: Reg<u32> = Reg::new(0x100);
/// Interrupt Clear-Enable Registers (32 IRQ на регистр).
pub(super) const GICD_ICENABLER: Reg<u32> = Reg::new(0x180);
/// Interrupt Clear-Pending Registers (32 IRQ на регистр).
pub(super) const GICD_ICPENDR: Reg<u32> = Reg::new(0x280);
/// Interrupt Priority Registers (4 IRQ на регистр, по 8 бит на приоритет).
pub(super) const GICD_IPRIORITYR: Reg<u32> = Reg::new(0x400);
/// Interrupt Configuration Registers (2 бита на IRQ).
pub(super) const GICD_ICFGR: Reg<u32> = Reg::new(0xC00);
/// Interrupt Group Modifier Registers.
pub(super) const GICD_IGRPMODR: Reg<u32> = Reg::new(0xD00);
/// Interrupt Routing Registers для SPI (64 бит на IRQ, начиная с IRQ 32).
pub(super) const GICD_IROUTER: Reg<u64> = Reg::new(0x6100);

/// Биты GICD_CTLR
/// Разрешить группу 1 Non-Secure.
pub(super) const GICD_CTLR_ENABLE_GRP1NS: u32 = 1 << 1;
/// Affinity Routing Enable для Non-Secure (ARE_NS) - бит 5 по спецификации GICv3.
pub(super) const GICD_CTLR_ARE_NS: u32 = 1 << 5;
/// Register Write Pending - бит 31, сигнализирует о незавершённой записи GICD_CTLR.
pub(super) const GICD_CTLR_RWP: u32 = 1 << 31;

/// Регистры Redistributor (GICR)
///
/// Каждый Redistributor содержит два фрейма по 64 KB:
/// - RD_base (смещение 0x00000): общие регистры
/// - SGI_base (смещение 0x10000): регистры для SGI/PPI

/// RD_base: Waker Register - управление переходом в сон/пробуждение.
pub(super) const GICR_WAKER: Reg<u32> = Reg::new(0x014);

/// Смещение SGI_base относительно RD_base.
pub(super) const GICR_SGI_BASE_OFFSET: usize = 0x10000;

/// SGI_base: Group Register для SGI/PPI (32 IRQ, биты 0-31).
pub(super) const GICR_IGROUPR0: Reg<u32> = Reg::new(GICR_SGI_BASE_OFFSET + 0x080);
/// SGI_base: Set-Enable Register для SGI/PPI.
pub(super) const GICR_ISENABLER0: Reg<u32> = Reg::new(GICR_SGI_BASE_OFFSET + 0x100);
/// SGI_base: Clear-Enable Register для SGI/PPI.
pub(super) const GICR_ICENABLER0: Reg<u32> = Reg::new(GICR_SGI_BASE_OFFSET + 0x180);
/// SGI_base: Priority Registers для SGI/PPI (8 бит на IRQ).
pub(super) const GICR_IPRIORITYR: Reg<u32> = Reg::new(GICR_SGI_BASE_OFFSET + 0x400);
/// SGI_base: Configuration Register для SGI.
pub(super) const GICR_ICFGR0: Reg<u32> = Reg::new(GICR_SGI_BASE_OFFSET + 0xC00);
/// SGI_base: Configuration Register для PPI.
pub(super) const GICR_ICFGR1: Reg<u32> = Reg::new(GICR_SGI_BASE_OFFSET + 0xC04);
/// SGI_base: Group Modifier Register для SGI/PPI.
pub(super) const GICR_IGRPMODR0: Reg<u32> = Reg::new(GICR_SGI_BASE_OFFSET + 0xD00);

/// Биты GICR_WAKER
/// Перевести процессор в режим сна.
pub(super) const GICR_WAKER_PROCESSOR_SLEEP: u32 = 1 << 1;
/// Флаг: все дочерние узлы находятся в спящем состоянии.
pub(super) const GICR_WAKER_CHILDREN_ASLEEP: u32 = 1 << 2;

/// Значение приоритетной маски, пропускающей все прерывания.
pub(super) const PRIORITY_MASK_ALL: u64 = 0xFF;

/// ITLinesNumber - количество банков по 32 IRQ.
pub(super) type ITLinesNumber = usize;

/// Тип прерывания в контексте GIC.
pub(super) enum IrqType {
    /// Software Generated Interrupt (0-15).
    Sgi,
    /// Private Peripheral Interrupt (16-31).
    Ppi,
    /// Shared Peripheral Interrupt (32-1019).
    Spi,
    /// Поддельное прерывание (1020-1023).
    Spurious,
}

impl IrqType {
    pub(super) const fn from_irq_number(irq: IrqNumber) -> Self {
        match irq.raw() {
            0..=15 => IrqType::Sgi,
            16..=31 => IrqType::Ppi,
            32..=1019 => IrqType::Spi,
            _ => IrqType::Spurious,
        }
    }
}

/// Вычисляет индекс регистра и позицию бита для заданного IRQ.
///
/// `bits` - ширина поля одного IRQ в регистре (1 для enable/disable, 8 для приоритетов).
#[inline]
pub(super) const fn bit_offset(irq: IrqNumber, bits: usize) -> (usize, usize) {
    let bank_width = u32::BITS as usize;
    let entries_per_bank = bank_width / bits;
    let raw = irq.raw() as usize;

    (raw / entries_per_bank, raw % entries_per_bank)
}
