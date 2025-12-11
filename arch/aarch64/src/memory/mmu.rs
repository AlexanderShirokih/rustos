use crate::memory::regs::common::EL1;
use crate::memory::regs::mair::MemoryAttributeIndirectionRegister;
use crate::memory::regs::sctrl::SystemControlRegister;
use crate::memory::regs::tcr::TranslationControlRegister;
use crate::memory::regs::tlb::TranslationLookasideBuffer;
use crate::memory::regs::ttbr::{TTBR0, TranslationTableBaseRegister};
use crate::memory::regs::{mair, sctrl, tcr};
use crate::system;
use memory::physical::PhysicalAddress;

/// Конфигурация для включения MMU.
pub trait MmuConfig {
    /// Физический адрес корня таблиц страниц (TTBR0_EL1).
    fn root_table(&self) -> PhysicalAddress;

    /// Значение MAIR_EL1.
    fn mair(&self) -> mair::MairBits {
        mair::MairBits::combine(&[
            mair::MairEntry::Normal(mair::NormalAttr::WbRaWa),
            mair::MairEntry::Device(mair::DeviceAttr::NgNre),
        ])
    }

    /// Значение TCR_EL1.
    fn tcr(&self) -> tcr::TCRBits {
        tcr::TCRBits::combine(&[
            tcr::TCR_T0SZ_48BIT,
            tcr::TCR_IRGN0_WB_WA,
            tcr::TCR_ORGN0_WB_WA,
            tcr::TCR_SH0_INNER,
            tcr::TCR_TG0_4K,
        ])
    }

    /// Какие биты нужно установить в SCTLR_EL1.
    fn sctlr(&self) -> sctrl::SctlrBits {
        sctrl::SctlrBits::combine(&[
            sctrl::SctlrBit::MmuEnable,
            sctrl::SctlrBit::DCacheEnable,
            sctrl::SctlrBit::ICacheEnable,
        ])
    }
}

#[derive(Debug, Clone, Copy)]
pub struct RootTableConfig {
    root_table: PhysicalAddress,
}

impl RootTableConfig {
    pub const fn new(root_table: PhysicalAddress) -> Self {
        Self { root_table }
    }
}

impl MmuConfig for RootTableConfig {
    fn root_table(&self) -> PhysicalAddress {
        self.root_table
    }
}

pub type EL1Mmu = Mmu<EL1>;

/// Операции с MMU.
pub struct Mmu<EL> {
    ttbr0: TranslationTableBaseRegister<EL, TTBR0>,
    tcr: TranslationControlRegister<EL>,
    mair: MemoryAttributeIndirectionRegister<EL>,
    tlb: TranslationLookasideBuffer<EL>,
    sctrl: SystemControlRegister<EL1>,
}

impl Mmu<EL1> {
    pub const fn new() -> Self {
        Self {
            ttbr0: TranslationTableBaseRegister::new(),
            tcr: TranslationControlRegister::new(),
            mair: MemoryAttributeIndirectionRegister::new(),
            tlb: TranslationLookasideBuffer::new(),
            sctrl: SystemControlRegister::new(),
        }
    }
}

impl Mmu<EL1> {
    /// Включить MMU/D-cache, используя подготовленную конфигурацию.
    pub fn enable<C: MmuConfig>(&self, config: C) {
        // 1) Барьер перед изменениями регистров
        system::barrier::barrier();

        // Маскируем прерывания
        system::interrupts::mask();

        // 2) Записываем корень таблицы
        self.ttbr0.set(config.root_table());

        // 3) Записываем регистры
        self.mair.set(config.mair());
        self.tcr.set(config.tcr());

        // 5) Сброс TLB
        self.tlb.invalidate();

        // 6) Включаем MMU + кэши
        self.sctrl.set(config.sctlr());

        // 7) Барьер после изменения всех регистров
        system::barrier::barrier();
    }
}
