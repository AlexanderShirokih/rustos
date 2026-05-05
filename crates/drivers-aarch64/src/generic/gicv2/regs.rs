//! Определения регистров GICv2 и вспомогательные типы.

use drivers_common::services::interrupts::IrqNumber;
use io::mmio::Reg;

/// Регистры Distributor interface

/// Control Register - включение distributor.
pub(super) const GICD_CTLR: Reg<u32> = Reg::new(0x000);
/// Type Register - количество поддерживаемых линий IRQ.
pub(super) const GICD_TYPER: Reg<u32> = Reg::new(0x004);
/// Interrupt Set-Enable Registers (32 IRQ на регистр).
pub(super) const GICD_ISENABLER: Reg<u32> = Reg::new(0x100);
/// Interrupt Clear-Enable Registers (32 IRQ на регистр).
pub(super) const GICD_ICENABLER: Reg<u32> = Reg::new(0x180);
/// Interrupt Clear-Pending Registers (32 IRQ на регистр).
pub(super) const GICD_ICPENDR: Reg<u32> = Reg::new(0x280);
/// Interrupt Priority Registers (4 IRQ на регистр, по 8 бит на приоритет).
pub(super) const GICD_IPRIORITYR: Reg<u32> = Reg::new(0x400);
/// Interrupt Processor Targets Registers (4 IRQ на регистр, по 8 бит на маску CPU).
pub(super) const GICD_ITARGETSR: Reg<u32> = Reg::new(0x800);
/// Interrupt Configuration Registers.
pub(super) const GICD_ICFGR: Reg<u32> = Reg::new(0xC00);

/// Регистры CPU Interface

/// CPU Interface Control Register - включение CPU interface.
pub(super) const GICC_CTLR: Reg<u32> = Reg::new(0x000);
/// Interrupt Priority Mask Register - фильтр приоритетов.
pub(super) const GICC_PMR: Reg<u32> = Reg::new(0x004);
/// Binary Point - Определяет разделение между группой приоритета и подприоритетом (0-7)
pub(super) const GICC_BPR: Reg<u32> = Reg::new(0x008);
/// Interrupt Acknowledge Register - чтение номера прерывания (Group 0, Secure view).
pub(super) const GICC_IAR: Reg<u32> = Reg::new(0x00C);
/// End of Interrupt Register - подтверждение обработки (Group 0, Secure view).
pub(super) const GICC_EOIR: Reg<u32> = Reg::new(0x010);

pub(super) const PRIORITY_MASK_ALL: u32 = 0xFF;

// ITLinesNumber = количество блоков по 32 IRQ каждый.
pub(super) type ITLinesNumber = usize;

/// Тип прерывания в контексте GIC.
pub(super) enum IrqType {
    /// Software Generated Interrupt (0-15).
    Sgi,
    /// Private Peripheral Interrupt (16-31).
    Ppi,
    /// Shared Peripheral Interrupt (32-1019).
    Spi,
    /// Spurious interrupt
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
