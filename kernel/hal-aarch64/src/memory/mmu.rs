//! Управление MMU AArch64.

use memory::physical_address::PhysicalAddress;

use crate::memory::regs::{
    common::{EL1, ExceptionLevel},
    id_aa64mmfr0::{AsidWidth, IdAa64Mmfr0},
    mair::MemoryAttributeIndirectionRegister,
    sctlr,
    sctlr::SystemControlRegister,
    tcr,
    tcr::{TranslationControlRegister, TtbrSel},
    tlb::TranslationLookasideBuffer,
    ttbr::{HigherHalf, LowerHalf, TranslationTableBaseRegister},
};

pub(crate) struct AddressSpaceConfig<T: TtbrSel> {
    base: PhysicalAddress,
    config: tcr::AddressTranslationConfig<T>,
}

pub(crate) trait MmuConfig {
    fn lower_half_config(&self, asid_width: AsidWidth) -> AddressSpaceConfig<LowerHalf>;

    fn higher_half_config(&self, asid_width: AsidWidth) -> AddressSpaceConfig<HigherHalf>;

    fn mmu_config(&self) -> sctlr::SctlrBits {
        sctlr::SctlrBits::combine(&[
            sctlr::SctlrBit::Mmu,
            sctlr::SctlrBit::DCache,
            sctlr::SctlrBit::ICache,
        ])
    }
}

pub(crate) struct NormalDualSpaceConfig {
    lower_root: PhysicalAddress,
    higher_root: PhysicalAddress,
}

impl NormalDualSpaceConfig {
    pub(crate) const fn new(lower_root: PhysicalAddress, higher_root: PhysicalAddress) -> Self {
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

pub(crate) struct Mmu<EL: ExceptionLevel> {
    _phantom: core::marker::PhantomData<EL>,
}

type Ttbr0 = TranslationTableBaseRegister<EL1, LowerHalf>;
type Ttbr1 = TranslationTableBaseRegister<EL1, HigherHalf>;
type Tcr = TranslationControlRegister<EL1>;
type Mair = MemoryAttributeIndirectionRegister<EL1>;
type Tlb = TranslationLookasideBuffer<EL1>;
type Sctlr = SystemControlRegister<EL1>;

impl Mmu<EL1> {
    // SAFETY: PC и SP уже в higher half, иначе код и стек теряют трансляцию. IRQ замаскированы.
    pub(crate) unsafe fn disable_lower_half() {
        Self::full_system_barrier();
        // SAFETY: IRQ замаскированы по контракту функции, и TCR_EL1 между чтением и записью
        // никто не пишет.
        unsafe { Tcr::disable_walks::<LowerHalf>() };
        Tlb::invalidate();
        Self::full_system_barrier();
    }

    pub(crate) fn enable<C: MmuConfig>(config: &C) {
        // 1) Барьер перед изменениями регистров + маскирование прерываний
        Self::full_system_barrier();

        // 2) Запись слотов атрибутов памяти
        Mair::set();

        // 3) Runtime-detect ширины ASID для TCR.AS. Глобальный аллокатор тегов
        // здесь не трогается, потому что этот путь проходит каждый CPU, а аллокатор
        // общий, и его настройка принадлежит загрузке boot CPU
        // (`asid::init_global`).
        let asid_width = IdAa64Mmfr0::asid_width();

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
        Self::full_system_barrier();
    }

    fn full_system_barrier() {
        // SAFETY: `dsb sy`/`isb` - общесистемный барьер на ARMv8, не имеет операндов и
        // не модифицирует регистры/память (preserves_flags, nostack).
        unsafe { core::arch::asm!("dsb sy", "isb", options(nostack, preserves_flags)) };
    }
}
