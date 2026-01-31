use crate::memory::regs::common::EL1;
use crate::memory::regs::mair::MemoryAttributeIndirectionRegister;
use crate::memory::regs::sctrl::SystemControlRegister;
use crate::memory::regs::tcr::{TranslationControlRegister, TtbrSel};
use crate::memory::regs::tlb::TranslationLookasideBuffer;
use crate::memory::regs::ttbr::{HigherHalf, LowerHalf, TranslationTableBaseRegister};
use crate::memory::regs::{mair, sctrl, tcr};
use crate::system;
use memory::physical_address::PhysicalAddress;

pub struct AddressSpaceConfig<T: TtbrSel> {
    /// Физический адрес корня таблиц страниц.
    base: PhysicalAddress,

    /// Конфигурация адресного пространства
    config: tcr::AddressTranslationConfig<T>,
}

/// Конфигурация для включения MMU.
pub trait MmuConfig {
    fn lower_half_config(&self) -> AddressSpaceConfig<LowerHalf>;

    /// Физический адрес корня таблиц страниц для верхней половины адресного пространства.
    fn higher_half_config(&self) -> AddressSpaceConfig<HigherHalf>;

    /// Свойства памяти для обычной памяти (RAM)
    fn normal_memory_config(&self) -> mair::NormalAttr {
        mair::NormalAttr::WbRaWa
    }

    /// Свойства памяти для памяти устройств (MMIO)
    fn device_memory_config(&self) -> mair::DeviceAttr {
        mair::DeviceAttr::NgNre
    }

    /// Конфигурация блока MMU
    fn mmu_config(&self) -> sctrl::SctlrBits {
        sctrl::SctlrBits::combine(&[
            sctrl::SctlrBit::MmuEnable,
            sctrl::SctlrBit::DCacheEnable,
            sctrl::SctlrBit::ICacheEnable,
        ])
    }
}

pub struct NormalDualSpaceConfig {
    lower_root: PhysicalAddress,
    higher_root: PhysicalAddress,
}

impl NormalDualSpaceConfig {
    pub const fn new(lower_root: PhysicalAddress, higher_root: PhysicalAddress) -> Self {
        Self {
            lower_root,
            higher_root,
        }
    }
}

impl MmuConfig for NormalDualSpaceConfig {
    fn lower_half_config(&self) -> AddressSpaceConfig<LowerHalf> {
        AddressSpaceConfig {
            base: self.lower_root,
            config: tcr::AddressTranslationConfig::create(true),
        }
    }

    fn higher_half_config(&self) -> AddressSpaceConfig<HigherHalf> {
        AddressSpaceConfig {
            base: self.higher_root,
            config: tcr::AddressTranslationConfig::create(true),
        }
    }
}

/// Операции с MMU.
pub struct Mmu<EL> {
    lower_half_base: TranslationTableBaseRegister<EL, LowerHalf>,
    higher_half_base: TranslationTableBaseRegister<EL, HigherHalf>,
    tcr: TranslationControlRegister<EL>,
    mair: MemoryAttributeIndirectionRegister<EL>,
    tlb: TranslationLookasideBuffer<EL>,
    sctlr: SystemControlRegister<EL1>,
}

impl Mmu<EL1> {
    pub const fn new() -> Self {
        Self {
            lower_half_base: TranslationTableBaseRegister::new(),
            higher_half_base: TranslationTableBaseRegister::new(),
            tcr: TranslationControlRegister::new(),
            mair: MemoryAttributeIndirectionRegister::new(),
            tlb: TranslationLookasideBuffer::new(),
            sctlr: SystemControlRegister::new(),
        }
    }
}

impl Mmu<EL1> {
    /// Включить MMU/D-cache, используя подготовленную конфигурацию.
    pub fn enable<C: MmuConfig>(&self, config: C) {
        // 1) Барьер перед изменениями регистров + Маскируем прерывания
        system::barrier::full_system_barrier();

        // 2) Записываем слоты атрибутов памяти
        self.mair.set(mair::MairBits::combine(&[
            mair::MairEntry::Normal(config.normal_memory_config()), // слот #0
            mair::MairEntry::Device(config.device_memory_config()), // слот #1
        ]));

        // 3) Записываем корень таблицы и конфигурацию адресации
        let lower_config = config.lower_half_config();
        let higher_config = config.higher_half_config();

        self.tcr.set(lower_config.config, higher_config.config);
        self.lower_half_base.set(lower_config.base);
        self.higher_half_base.set(higher_config.base);

        // 4) Сброс TLB
        self.tlb.invalidate();

        // 5) Включаем MMU + кэши
        self.sctlr.set(config.mmu_config());

        // 6) Барьер после изменения всех регистров
        system::barrier::full_system_barrier();
    }
}
