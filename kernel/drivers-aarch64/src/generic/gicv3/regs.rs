//! Определения MMIO-регистров GICv3 и вспомогательные типы.

use drivers_common::services::interrupts::IrqNumber;
use drivers_common_aarch64::{affinity_frames::FrameOwner, ipi_affinity::IpiAffinity};
use io::mmio::Reg;

// Регистры Distributor (GICD)

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
/// Affinity Routing Enable, Non-Secure (ARE_NS) - бит 4 в Non-secure view (в Secure - 5).
pub(super) const GICD_CTLR_ARE_NS: u32 = 1 << 4;
/// Register Write Pending - бит 31, сигнализирует о незавершённой записи GICD_CTLR.
pub(super) const GICD_CTLR_RWP: u32 = 1 << 31;

// Регистры Redistributor (GICR)
//
// Каждый Redistributor содержит два фрейма по 64 KB:
//   RD_base (смещение 0x00000): общие регистры
//   SGI_base (смещение 0x10000): регистры для SGI/PPI

/// RD_base: Waker Register - управление переходом в сон/пробуждение.
pub(super) const GICR_WAKER: Reg<u32> = Reg::new(0x014);

/// RD_base: Type Register - affinity владельца фрейма и признак последнего.
pub(super) const GICR_TYPER: Reg<u64> = Reg::new(0x008);

/// `GICR_TYPER.Last`: фрейм последний в непрерывной области.
const GICR_TYPER_LAST: u64 = 1 << 4;

/// Раскладывает `GICR_TYPER` на владельца фрейма.
///
/// `Affinity_Value` (биты [63:32]) держит уровни четырьмя байтами подряд,
/// `Aff0` младшим, а в `MPIDR_EL1` между `Aff3` и `Aff2` лежат служебные биты.
pub(super) const fn frame_owner(typer: u64) -> FrameOwner {
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

// Aff3=4, Aff2=3, Aff1=2, Aff0=1.
const _: () = assert!(frame_owner(0x0403_0201_0000_0000).affinity.aff3 == 4);
const _: () = assert!(frame_owner(0x0403_0201_0000_0000).affinity.aff2 == 3);
const _: () = assert!(frame_owner(0x0403_0201_0000_0000).affinity.aff1 == 2);
const _: () = assert!(frame_owner(0x0403_0201_0000_0000).affinity.aff0 == 1);
const _: () = assert!(!frame_owner(0x0403_0201_0000_0000).last);
const _: () = assert!(frame_owner(GICR_TYPER_LAST).last);

/// Расстояние между фреймами соседних Redistributor'ов по умолчанию: два
/// фрейма по 64 KB. Device tree вправе объявить другое через
/// `redistributor-stride`.
pub const GICR_DEFAULT_STRIDE: usize = 0x2_0000;

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

/// Сколько байт фрейма занимают программируемые драйвером регистры.
/// `GICR_IGRPMODR0` - самый дальний из них.
pub(super) const GICR_FRAME_SPAN: usize = GICR_SGI_BASE_OFFSET + 0xD00 + size_of::<u32>();

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
