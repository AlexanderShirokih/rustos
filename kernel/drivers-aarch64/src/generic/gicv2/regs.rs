//! Смещения регистров GICD/GICC, которые открывает `controller`, и `IrqType` -
//! классификация номера прерывания на SGI/PPI/SPI/spurious по диапазону.

use drivers_common::services::interrupts::IrqNumber;
use io::mmio::Reg;

// Регистры Distributor interface

pub(super) const GICD_CTLR: Reg<u32> = Reg::new(0x000);
pub(super) const GICD_TYPER: Reg<u32> = Reg::new(0x004);
pub(super) const GICD_ISENABLER: Reg<u32> = Reg::new(0x100);
pub(super) const GICD_ICENABLER: Reg<u32> = Reg::new(0x180);
pub(super) const GICD_ICPENDR: Reg<u32> = Reg::new(0x280);
pub(super) const GICD_IPRIORITYR: Reg<u32> = Reg::new(0x400);
pub(super) const GICD_ITARGETSR: Reg<u32> = Reg::new(0x800);
pub(super) const GICD_ICFGR: Reg<u32> = Reg::new(0xC00);
pub(super) const GICD_SGIR: Reg<u32> = Reg::new(0xF00);

// Регистры CPU Interface

pub(super) const GICC_CTLR: Reg<u32> = Reg::new(0x000);
pub(super) const GICC_PMR: Reg<u32> = Reg::new(0x004);
pub(super) const GICC_BPR: Reg<u32> = Reg::new(0x008);
pub(super) const GICC_IAR: Reg<u32> = Reg::new(0x00C);
pub(super) const GICC_EOIR: Reg<u32> = Reg::new(0x010);

pub(super) const PRIORITY_MASK_ALL: u32 = 0xFF;

pub(super) const CTLR_DISABLE: u32 = 0;

pub(super) const CTLR_ENABLE_GRP0: u32 = 1;

pub(super) const BINARY_POINT_NO_SUBPRIORITY: u32 = 0;

pub(super) type IrqBankCount = usize;

pub(super) const SPI_BASE: u16 = 32;

pub(super) const LINES_PER_BANK: usize = 32;

pub(super) const LAST_SPI: u16 = 1019;

pub(super) enum IrqType {
    Sgi,
    Ppi,
    Spi,
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

#[inline]
pub(super) const fn bit_offset(irq: IrqNumber, bits_per_irq: usize) -> (usize, usize) {
    let bank_width = u32::BITS as usize;
    let entries_per_bank = bank_width / bits_per_irq;
    let raw = irq.raw() as usize;

    (raw / entries_per_bank, raw % entries_per_bank)
}
