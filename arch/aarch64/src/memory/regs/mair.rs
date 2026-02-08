//! Memory Attribute Indirection Register
//! Регистр, который описывает типы памяти (cacheable, device, write-back).
//!
use crate::combine_bits;
use crate::memory::regs::common::EL1;
use core::arch::asm;

/// Атрибуты обычной памяти (Normal).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum NormalAttr {
    /// Write-Back, Read/Write-Allocate.
    WbRaWa = 0xFF,
}

/// Атрибуты памяти устройств (Device).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum DeviceAttr {
    /// Device-nGnRE.
    NgNre = 0x04,
}

/// Запись MAIR (тип памяти).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MairEntry {
    /// Обычная память.
    Normal(NormalAttr),
    /// Память устройств.
    Device(DeviceAttr),
}

impl MairEntry {
    const fn encode(self) -> u64 {
        match self {
            MairEntry::Normal(attr) => attr as u64,
            MairEntry::Device(attr) => (attr as u64) << 8,
        }
    }
}

/// Скомбинированные биты MAIR.
pub struct MairBits(u64);

impl MairBits {
    /// Объединяет записи MAIR в одно значение.
    pub const fn combine(bits: &[MairEntry]) -> MairBits {
        MairBits(combine_bits!(bits))
    }
}

/// Регистр MAIR (Memory Attribute Indirection Register).
pub struct MemoryAttributeIndirectionRegister<EL> {
    _phantom: core::marker::PhantomData<EL>,
}

impl MemoryAttributeIndirectionRegister<EL1> {
    pub const fn new() -> Self {
        Self {
            _phantom: core::marker::PhantomData,
        }
    }

    pub fn set(&self, value: MairBits) {
        unsafe { asm!("msr mair_el1, {0}", in(reg) value.0, options(nostack, preserves_flags)) };
    }
}
