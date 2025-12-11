//! Memory Attribute Indirection Register
//! Регистр, который описывает типы памяти (cacheable, device, write-back).
//!
use crate::combine_bits;
use crate::memory::regs::common::EL1;
use core::arch::asm;

/// Кодировки **Normal memory** для MAIR attribute byte.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum NormalAttr {
    /// Inner/Outer Write-Back, Read/Write-Allocate.
    WbRaWa = 0xFF,
}

/// Кодировки **Device memory** для MAIR attribute byte.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum DeviceAttr {
    /// Device-nGnRE.
    NgNre = 0x04,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MairEntry {
    Normal(NormalAttr),
    Device(DeviceAttr),
}

impl MairEntry {
    const fn encode(self) -> u64 {
        match self {
            MairEntry::Normal(attr) => (attr as u64) << 0,
            MairEntry::Device(attr) => (attr as u64) << 8,
        }
    }
}

pub struct MairBits(u64);

impl MairBits {
    pub const fn combine(bits: &[MairEntry]) -> MairBits {
        MairBits(combine_bits!(bits))
    }
}

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
