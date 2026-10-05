//! Смещения регистров GICD/GICR, которые открывает `controller`, раскладка бит
//! `GICR_TYPER` в [`FrameOwner`] и `IrqType` - классификация номера прерывания.

use drivers_common::services::interrupts::IrqNumber;
use drivers_common_aarch64::{affinity_frames::FrameOwner, ipi_affinity::IpiAffinity};
use io::mmio::Reg;

// Регистры Distributor (GICD)

pub(super) const GICD_CTLR: Reg<u32> = Reg::new(0x000);
pub(super) const GICD_TYPER: Reg<u32> = Reg::new(0x004);
pub(super) const GICD_IGROUPR: Reg<u32> = Reg::new(0x080);
pub(super) const GICD_ISENABLER: Reg<u32> = Reg::new(0x100);
pub(super) const GICD_ICENABLER: Reg<u32> = Reg::new(0x180);
pub(super) const GICD_ICPENDR: Reg<u32> = Reg::new(0x280);
pub(super) const GICD_IPRIORITYR: Reg<u32> = Reg::new(0x400);
pub(super) const GICD_ICFGR: Reg<u32> = Reg::new(0xC00);
pub(super) const GICD_IGRPMODR: Reg<u32> = Reg::new(0xD00);
pub(super) const GICD_IROUTER: Reg<u64> = Reg::new(0x6100);

// Биты GICD_CTLR. ARE_NS занимает бит 4 в Non-secure view и бит 5 в Secure.

pub(super) const GICD_CTLR_DISABLE: u32 = 0;
pub(super) const GICD_CTLR_ENABLE_GRP1NS: u32 = 1 << 1;
pub(super) const GICD_CTLR_ARE_NS: u32 = 1 << 4;
pub(super) const GICD_CTLR_RWP: u32 = 1 << 31;

// Регистры Redistributor (GICR)
//
// Каждый Redistributor содержит два фрейма по 64 KB:
//   RD_base (смещение 0x00000): общие регистры
//   SGI_base (смещение 0x10000): регистры для SGI/PPI

pub(super) const GICR_WAKER: Reg<u32> = Reg::new(0x014);

pub(super) const GICR_TYPER: Reg<u64> = Reg::new(0x008);

const GICR_TYPER_LAST: u64 = 1 << 4;

pub(super) const fn frame_owner(typer: u64) -> FrameOwner {
    // `GICR_TYPER.Affinity_Value` (биты [63:32]) держит уровни четырьмя байтами
    // подряд, `Aff0` младшим. В `MPIDR_EL1` между `Aff3` и `Aff2` лежат служебные
    // биты, поэтому раскладка здесь своя.
    let value = (typer >> 32) as u32;

    FrameOwner {
        affinity: IpiAffinity {
            aff0: (value & 0xFF) as u8,
            aff1: ((value >> 8) & 0xFF) as u8,
            aff2: ((value >> 16) & 0xFF) as u8,
            aff3: ((value >> 24) & 0xFF) as u8,
        },
        last: typer & GICR_TYPER_LAST != 0,
    }
}

const _: () = assert!(frame_owner(0x0403_0201_0000_0000).affinity.aff3 == 4);
const _: () = assert!(frame_owner(0x0403_0201_0000_0000).affinity.aff2 == 3);
const _: () = assert!(frame_owner(0x0403_0201_0000_0000).affinity.aff1 == 2);
const _: () = assert!(frame_owner(0x0403_0201_0000_0000).affinity.aff0 == 1);
const _: () = assert!(!frame_owner(0x0403_0201_0000_0000).last);
const _: () = assert!(frame_owner(GICR_TYPER_LAST).last);

pub(crate) const GICR_DEFAULT_STRIDE: usize = 0x2_0000;

pub(super) const GICR_SGI_BASE_OFFSET: usize = 0x10000;

pub(super) const GICR_IGROUPR0: Reg<u32> = Reg::new(GICR_SGI_BASE_OFFSET + 0x080);
pub(super) const GICR_ISENABLER0: Reg<u32> = Reg::new(GICR_SGI_BASE_OFFSET + 0x100);
pub(super) const GICR_ICENABLER0: Reg<u32> = Reg::new(GICR_SGI_BASE_OFFSET + 0x180);
pub(super) const GICR_IPRIORITYR: Reg<u32> = Reg::new(GICR_SGI_BASE_OFFSET + 0x400);
pub(super) const GICR_ICFGR0: Reg<u32> = Reg::new(GICR_SGI_BASE_OFFSET + 0xC00);
pub(super) const GICR_ICFGR1: Reg<u32> = Reg::new(GICR_SGI_BASE_OFFSET + 0xC04);
pub(super) const GICR_IGRPMODR0: Reg<u32> = Reg::new(GICR_SGI_BASE_OFFSET + 0xD00);

// Самый дальний из программируемых драйвером регистров фрейма - GICR_IGRPMODR0.

pub(super) const GICR_FRAME_SPAN: usize = GICR_SGI_BASE_OFFSET + 0xD00 + size_of::<u32>();

// Биты GICR_WAKER

pub(super) const GICR_WAKER_PROCESSOR_SLEEP: u32 = 1 << 1;
pub(super) const GICR_WAKER_CHILDREN_ASLEEP: u32 = 1 << 2;

pub(super) const PRIORITY_MASK_ALL: u64 = 0xFF;

pub(super) const PRIORITY_MASK_ALL_BANK: u32 = 0xFFFF_FFFF;

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
