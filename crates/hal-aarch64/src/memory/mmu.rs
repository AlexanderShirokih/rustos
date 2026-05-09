//! Управление MMU AArch64.

use memory::physical_address::PhysicalAddress;

use crate::{
    memory::{
        asid,
        regs::{
            common::EL1,
            id_aa64mmfr0::{AsidWidth, IdAa64Mmfr0},
            mair,
            mair::MemoryAttributeIndirectionRegister,
            sctlr,
            sctlr::SystemControlRegister,
            tcr,
            tcr::{TranslationControlRegister, TtbrSel},
            tlb::TranslationLookasideBuffer,
            ttbr::{HigherHalf, LowerHalf, TranslationTableBaseRegister},
        },
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
    fn lower_half_config(&self, asid_width: AsidWidth) -> AddressSpaceConfig<LowerHalf>;

    /// Физический адрес корня таблиц страниц для верхней половины адресного пространства.
    fn higher_half_config(&self, asid_width: AsidWidth) -> AddressSpaceConfig<HigherHalf>;

    /// Свойства памяти для обычной памяти (RAM)
    fn normal_memory_config(&self) -> mair::NormalAttr {
        mair::NormalAttr::WbRaWa
    }

    /// Свойства памяти для памяти устройств (MMIO)
    fn device_memory_config(&self) -> mair::DeviceAttr {
        mair::DeviceAttr::NgNre
    }

    /// Конфигурация блока MMU
    fn mmu_config(&self) -> sctlr::SctlrBits {
        sctlr::SctlrBits::combine(&[
            sctlr::SctlrBit::Mmu,
            sctlr::SctlrBit::DCache,
            sctlr::SctlrBit::ICache,
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
    fn lower_half_config(&self, asid_width: AsidWidth) -> AddressSpaceConfig<LowerHalf> {
        AddressSpaceConfig {
            base: self.lower_root,
            config: tcr::AddressTranslationConfig::create(true, asid_width),
        }
    }

    fn higher_half_config(&self, asid_width: AsidWidth) -> AddressSpaceConfig<HigherHalf> {
        AddressSpaceConfig {
            base: self.higher_root,
            config: tcr::AddressTranslationConfig::create(true, asid_width),
        }
    }
}

/// Операции с MMU.
pub struct Mmu<EL> {
    _phantom: core::marker::PhantomData<EL>,
}

type Ttbr0 = TranslationTableBaseRegister<EL1, LowerHalf>;
type Ttbr1 = TranslationTableBaseRegister<EL1, HigherHalf>;
type Tcr = TranslationControlRegister<EL1>;
type Mair = MemoryAttributeIndirectionRegister<EL1>;
type Tlb = TranslationLookasideBuffer<EL1>;
type Sctlr = SystemControlRegister<EL1>;

impl Mmu<EL1> {
    /// Отключает identity mapping: обнуляет TTBR0_EL1 и сбрасывает TLB.
    ///
    /// Вызывать только после перехода в higher half (виртуальный SP и PC).
    pub fn disable_lower_half() {
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
        Tlb::invalidate();
        system::barrier::full_system_barrier();
    }

    /// Включает MMU и кэши.
    pub fn enable<C: MmuConfig>(config: &C) {
        // 1) Барьер перед изменениями регистров + маскирование прерываний
        system::barrier::full_system_barrier();

        // 2) Запись слотов атрибутов памяти
        Mair::set(mair::MairBits::combine(&[
            mair::MairEntry::Normal(config.normal_memory_config()), // слот #0
            mair::MairEntry::Device(config.device_memory_config()), // слот #1
        ]));

        // 3) Runtime-detect ширины ASID - она прокидывается в TCR.AS и в
        // глобальный аллокатор тегов, чтобы оба видели одинаковый предел.
        let asid_width = IdAa64Mmfr0::asid_width();
        asid::init(asid_width);

        // 4) Запись корня таблицы и конфигурации адресации
        let lower_config = config.lower_half_config(asid_width);
        let higher_config = config.higher_half_config(asid_width);

        Tcr::set(lower_config.config, higher_config.config);
        Ttbr0::set(lower_config.base);
        Ttbr1::set(higher_config.base);

        // 5) Сброс TLB
        Tlb::invalidate();

        // 6) Включение MMU + кэшей
        Sctlr::set(config.mmu_config());

        // 7) Барьер после изменения всех регистров
        system::barrier::full_system_barrier();
    }
}
