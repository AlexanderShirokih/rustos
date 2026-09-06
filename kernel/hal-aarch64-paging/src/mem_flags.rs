//! Атрибуты памяти для записей таблицы страниц.

use core::fmt::{Debug, Formatter};

use memory::{
    MemFlags,
    mem_flags::{AccessMode, Executable, Owners},
};

use crate::memory_attribute::MemoryAttribute;

/// Атрибуты памяти страницы/блока.
#[repr(transparent)]
#[derive(Copy, Clone, Eq, PartialEq)]
pub struct Aarch64MemFlags(u64);

const ATTR_IDX_SHIFT: u32 = 2;
const ATTR_IDX_MASK: u64 = 0b111;

const AP_SHIFT: u32 = 6;
const AP_MASK: u64 = 0b11;

const SH_SHIFT: u32 = 8;
const SH_MASK: u64 = 0b11;

const AF_BIT: u64 = 1 << 10;
const NG_BIT: u64 = 1 << 11;
const PXN_BIT: u64 = 1 << 53;
const UXN_BIT: u64 = 1 << 54;

/// Режим совместного доступа к памяти (SH).
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Shareability {
    /// Не разделяемая.
    None = 0b00,
    /// Зарезервировано.
    Reserved = 0b01,
    /// Outer Shareable.
    Outer = 0b10,
    /// Inner Shareable.
    Inner = 0b11,
}

/// Права доступа (`AP[2:1]`).
///
/// Кодировка соответствует ARMv8-A Architecture Reference Manual (D5.5.3,
/// stage-1 attributes table). Раньше в проекте значения `KernelRO` и `UserRW`
/// стояли переставлены местами относительно стандарта. `UserRW`-маппинг на
/// практике становился `KernelRO`, и user-mode получал permission fault при
/// первой же инструкции.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Access {
    /// EL1 RW, EL0 нет доступа.
    KernelRW = 0b00,
    /// EL1 RW, EL0 RW.
    UserRW = 0b01,
    /// EL1 RO, EL0 нет доступа.
    KernelRO = 0b10,
    /// EL1 RO, EL0 RO.
    UserRO = 0b11,
}

impl Shareability {
    const fn from_bits(bits: u64) -> Self {
        match bits & SH_MASK {
            0b00 => Self::None,
            0b01 => Self::Reserved,
            0b10 => Self::Outer,
            _ => Self::Inner,
        }
    }
}

impl Access {
    const fn from_bits(bits: u64) -> Self {
        match bits & AP_MASK {
            0b00 => Self::KernelRW,
            0b01 => Self::UserRW,
            0b10 => Self::KernelRO,
            _ => Self::UserRO,
        }
    }
}

impl Aarch64MemFlags {
    const fn field(self, shift: u32, mask: u64, val: u64) -> Self {
        Self((self.0 & !(mask << shift)) | ((val & mask) << shift))
    }

    const fn get_field(self, shift: u32, mask: u64) -> u64 {
        (self.0 >> shift) & mask
    }

    const fn bit(self, mask: u64, on: bool) -> Self {
        Self(if on { self.0 | mask } else { self.0 & !mask })
    }

    const fn get_bit(self, mask: u64) -> bool {
        self.0 & mask != 0
    }
}

impl Aarch64MemFlags {
    pub const fn new() -> Self {
        Self(0)
    }

    pub const fn from_memflags(flag: MemFlags) -> Self {
        match flag {
            MemFlags::Cached(Owners { kernel, user }) => Aarch64MemFlags::new()
                .attr_index(MemoryAttribute::Cached.index())
                .sh(Shareability::Inner)
                .ap(Self::map_access_flags(kernel.access, user.access))
                .af(true)
                .pxn(matches!(kernel.executable, Executable::NotAllowed))
                .uxn(matches!(user.executable, Executable::NotAllowed)),

            MemFlags::WriteCombining(Owners { kernel, user }) => Aarch64MemFlags::new()
                .attr_index(MemoryAttribute::WriteCombining.index())
                .sh(Shareability::Inner)
                .ap(Self::map_access_flags(kernel.access, user.access))
                .af(true)
                .pxn(true)
                .uxn(true),

            MemFlags::Device(Owners { kernel, user }) => Aarch64MemFlags::new()
                .attr_index(MemoryAttribute::Device.index())
                .sh(Shareability::Inner)
                .ap(Self::map_access_flags(kernel.access, user.access))
                .af(true)
                .pxn(true)
                .uxn(true),
        }
    }

    const fn map_access_flags(kernel: AccessMode, user: AccessMode) -> Access {
        match (kernel, user) {
            (AccessMode::Writable, AccessMode::Readonly) => {
                panic!("Invalid AF combination: kRW+uR");
            }

            (_, AccessMode::Writable) => Access::UserRW,

            (_, AccessMode::Readonly) => Access::UserRO,

            (AccessMode::Writable, AccessMode::None) => Access::KernelRW,

            (_, AccessMode::None) => Access::KernelRO,
        }
    }

    pub const fn from_bits(bits: u64) -> Self {
        Self(bits)
    }

    pub const fn bits(self) -> u64 {
        self.0
    }

    pub const fn af(self, on: bool) -> Self {
        self.bit(AF_BIT, on)
    }

    pub const fn ng(self, on: bool) -> Self {
        self.bit(NG_BIT, on)
    }

    pub const fn pxn(self, on: bool) -> Self {
        self.bit(PXN_BIT, on)
    }

    pub const fn uxn(self, on: bool) -> Self {
        self.bit(UXN_BIT, on)
    }

    pub const fn sh(self, sh: Shareability) -> Self {
        self.field(SH_SHIFT, SH_MASK, sh as u64)
    }

    pub const fn ap(self, ap: Access) -> Self {
        self.field(AP_SHIFT, AP_MASK, ap as u64)
    }

    /// Устанавливает индекс MAIR для атрибутов памяти. Значение шире трёх бит молча
    /// обрезается. Годный `idx` даёт [`MemoryAttribute::index`].
    pub const fn attr_index(self, idx: u8) -> Self {
        self.field(ATTR_IDX_SHIFT, ATTR_IDX_MASK, idx as u64)
    }

    pub const fn get_af(self) -> bool {
        self.get_bit(AF_BIT)
    }

    pub const fn get_ng(self) -> bool {
        self.get_bit(NG_BIT)
    }

    pub const fn get_pxn(self) -> bool {
        self.get_bit(PXN_BIT)
    }

    pub const fn get_uxn(self) -> bool {
        self.get_bit(UXN_BIT)
    }

    pub const fn get_sh(self) -> Shareability {
        Shareability::from_bits(self.get_field(SH_SHIFT, SH_MASK))
    }

    pub const fn get_ap(self) -> Access {
        Access::from_bits(self.get_field(AP_SHIFT, AP_MASK))
    }

    pub const fn get_attr_index(self) -> u8 {
        self.get_field(ATTR_IDX_SHIFT, ATTR_IDX_MASK) as u8
    }
}

impl Default for Aarch64MemFlags {
    fn default() -> Self {
        Self::new()
    }
}

impl Debug for Aarch64MemFlags {
    fn fmt(&self, f: &mut Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Aarch64MemFlags")
            .field("sh", &self.get_sh())
            .field("ap", &self.get_ap())
            .field("attr_idx", &self.get_attr_index())
            .field("af", &self.get_af())
            .field("ng", &self.get_ng())
            .field("pxn", &self.get_pxn())
            .field("uxn", &self.get_uxn())
            .finish_non_exhaustive()
    }
}
