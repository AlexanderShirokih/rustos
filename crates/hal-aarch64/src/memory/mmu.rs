//! Управление MMU AArch64.

use memory::physical_address::PhysicalAddress;

use crate::{
    memory::regs::{
        common::EL1,
        mair,
        mair::MemoryAttributeIndirectionRegister,
        sctrl,
        sctrl::SystemControlRegister,
        tcr,
        tcr::{TranslationControlRegister, TtbrSel},
        tlb::TranslationLookasideBuffer,
        ttbr::{HigherHalf, LowerHalf, TranslationTableBaseRegister},
    },
    system,
};

/// Конфигурация адресного пространства.
pub struct AddressSpaceConfig<T: TtbrSel> {
    /// Физический адрес корня таблиц страниц.
    base: PhysicalAddress,
    /// Параметры трансляции.
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
            sctrl::SctlrBit::Mmu,
            sctrl::SctlrBit::DCache,
            sctrl::SctlrBit::ICache,
        ])
    }
}

/// Стандартная конфигурация с двумя адресными пространствами.
pub struct NormalDualSpaceConfig {
    /// Корень lower half (TTBR0).
    lower_root: PhysicalAddress,
    /// Корень higher half (TTBR1).
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
    /// Регистр TTBR0 (lower half).
    lower_half_base: TranslationTableBaseRegister<EL, LowerHalf>,
    /// Регистр TTBR1 (higher half).
    higher_half_base: TranslationTableBaseRegister<EL, HigherHalf>,
    /// Регистр TCR (параметры трансляции).
    tcr: TranslationControlRegister<EL>,
    /// Регистр MAIR (атрибуты памяти).
    mair: MemoryAttributeIndirectionRegister<EL>,
    /// TLB (кэш трансляций).
    tlb: TranslationLookasideBuffer<EL>,
    /// Регистр SCTLR (управление системой).
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
    /// Отключает identity mapping: обнуляет TTBR0_EL1 и сбрасывает TLB.
    ///
    /// Вызывать только после перехода в higher half (виртуальный SP и PC).
    pub fn disable_lower_half(&self) {
        system::barrier::full_system_barrier();
        // SAFETY: Вызывается post-MMU, после переключения SP и PC на виртуальные адреса.
        // После этого вызова любое обращение к lower half вызовет Translation Fault.
        unsafe {
            core::arch::asm!(
                "msr ttbr0_el1, xzr",
                "isb",
                options(nostack, preserves_flags)
            );
        }
        self.tlb.invalidate();
        system::barrier::full_system_barrier();
    }

    /// Включает MMU и кэши.
    pub fn enable<C: MmuConfig>(&self, config: C) {
        // 1) Барьер перед изменениями регистров + маскирование прерываний
        system::barrier::full_system_barrier();

        // 2) Запись слотов атрибутов памяти
        self.mair.set(mair::MairBits::combine(&[
            mair::MairEntry::Normal(config.normal_memory_config()), // слот #0
            mair::MairEntry::Device(config.device_memory_config()), // слот #1
        ]));

        // 3) Запись корня таблицы и конфигурации адресации
        let lower_config = config.lower_half_config();
        let higher_config = config.higher_half_config();

        self.tcr.set(lower_config.config, higher_config.config);
        self.lower_half_base.set(lower_config.base);
        self.higher_half_base.set(higher_config.base);

        // 4) Сброс TLB
        self.tlb.invalidate();

        // 5) Включение MMU + кэшей
        self.sctlr.set(config.mmu_config());

        // 6) Барьер после изменения всех регистров
        system::barrier::full_system_barrier();
    }
}
