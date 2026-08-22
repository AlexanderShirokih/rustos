//! Memory Attribute Indirection Register
//!
//! Регистр, который описывает типы памяти (cacheable, device, write-back).
//! Состав слотов задаёт `hal_aarch64_paging::memory_attribute`.

use hal_aarch64_paging::memory_attribute::MemoryAttribute;

use crate::{
    memory::regs::common::{EL1, ExceptionLevel},
    write_sysreg,
};

/// Регистр MAIR (Memory Attribute Indirection Register).
pub struct MemoryAttributeIndirectionRegister<EL: ExceptionLevel> {
    _phantom: core::marker::PhantomData<EL>,
}

impl MemoryAttributeIndirectionRegister<EL1> {
    /// Программирует все объявленные слоты атрибутов памяти.
    pub fn set() {
        // SAFETY: Запись в MAIR_EL1 допустима на EL1 до включения MMU.
        // Значение собрано из типобезопасной таблицы слотов `MemoryAttribute`.
        unsafe { write_sysreg!(mair_el1, MemoryAttribute::mair_value()) };
    }
}
