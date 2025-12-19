use crate::aligned::{Address, Aligned};
use core::fmt::{Debug, Display, Formatter};

impl Address for PhysicalAddress {
    #[inline]
    fn as_usize(self) -> usize {
        self.0
    }

    #[inline]
    fn as_physical_address(self) -> PhysicalAddress {
        self
    }
}

/// Адрес физической памяти
#[repr(transparent)]
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct PhysicalAddress(usize);

impl PhysicalAddress {
    pub const fn new(address: usize) -> Self {
        Self(address)
    }

    pub const fn as_usize(self) -> usize {
        self.0
    }

    pub const fn as_u64(self) -> u64 {
        self.0 as u64
    }

    pub const fn align_down(self, frame_size: usize) -> PageAlignedAddress {
        debug_assert!(frame_size == PageAlignedAddress::ALIGNMENT);

        PageAlignedAddress::aligned_down(self)
    }

    pub const fn align_up(self, frame_size: usize) -> PageAlignedAddress {
        debug_assert!(frame_size == PageAlignedAddress::ALIGNMENT);

        PageAlignedAddress::aligned_up(self)
    }

    #[inline]
    pub const fn add(&self, x: usize) -> Self {
        Self(self.0 + x)
    }

    /// Добавляет значение с проверкой переполнения
    #[inline]
    pub fn checked_add(&self, x: usize) -> Option<Self> {
        self.0.checked_add(x).map(Self)
    }

    /// Вычитает значение с проверкой переполнения
    #[inline]
    pub fn checked_sub(&self, x: usize) -> Option<Self> {
        self.0.checked_sub(x).map(Self)
    }

    #[inline]
    pub fn sub(self, x: usize) -> Self {
        Self(self.0 - x)
    }
}

impl Display for PhysicalAddress {
    fn fmt(&self, f: &mut Formatter<'_>) -> core::fmt::Result {
        f.write_fmt(format_args!("0x{:x}", self.0))
    }
}
impl Debug for PhysicalAddress {
    fn fmt(&self, f: &mut Formatter<'_>) -> core::fmt::Result {
        write!(f, "PhysicalAddress({})", self)
    }
}

impl From<usize> for PhysicalAddress {
    fn from(v: usize) -> Self {
        Self(v)
    }
}

/// Выровненный физический адрес
/// Гарантирует, что адрес выровнен на заданную границу
#[repr(transparent)]
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct AlignedPhysicalAddress<const SHIFT: u8>(usize);

impl<const SHIFT: u8> AlignedPhysicalAddress<SHIFT> {
    pub const fn as_usize(self) -> usize {
        self.0
    }

    pub fn new(address: PhysicalAddress) -> Option<Self> {
        if address.0 % Self::ALIGNMENT == 0 {
            Some(Self(address.0))
        } else {
            None
        }
    }

    pub fn from_usize(address: usize) -> Option<Self> {
        if address % Self::ALIGNMENT == 0 {
            Some(Self(address))
        } else {
            None
        }
    }

    /// Создаёт выровненный адрес без проверки выравнивания
    pub const fn new_unchecked(address: PhysicalAddress) -> Self {
        Self(address.0)
    }

    /// Создаёт выровненный адрес, выравнивая вниз
    pub const fn aligned_down(address: PhysicalAddress) -> Self {
        let aligned = (address.0 / Self::ALIGNMENT) * Self::ALIGNMENT;
        Self(aligned)
    }

    /// Создаёт выровненный адрес, выравнивая вверх
    pub const fn aligned_up(address: PhysicalAddress) -> Self {
        let aligned = address.0.div_ceil(Self::ALIGNMENT) * Self::ALIGNMENT;
        Self(aligned)
    }

    pub const fn alignment(&self) -> usize {
        Self::ALIGNMENT
    }
}

impl<const SHIFT: u8> Display for AlignedPhysicalAddress<SHIFT> {
    fn fmt(&self, f: &mut Formatter<'_>) -> core::fmt::Result {
        f.write_fmt(format_args!("0x{:x}", self.0))
    }
}

impl<const SHIFT: u8> Debug for AlignedPhysicalAddress<SHIFT> {
    fn fmt(&self, f: &mut Formatter<'_>) -> core::fmt::Result {
        write!(f, "AlignedPhysicalAddress({})", self)
    }
}

impl<const SHIFT: u8> Address for AlignedPhysicalAddress<SHIFT> {
    #[inline]
    fn as_usize(self) -> usize {
        self.0
    }

    #[inline]
    fn as_physical_address(self) -> PhysicalAddress {
        PhysicalAddress(self.0)
    }
}

impl<const SHIFT: u8> Aligned for AlignedPhysicalAddress<SHIFT> {
    const ALIGNMENT: usize = 1usize << SHIFT;
}

impl<const SHIFT: u8> From<AlignedPhysicalAddress<SHIFT>> for PhysicalAddress {
    fn from(addr: AlignedPhysicalAddress<SHIFT>) -> Self {
        PhysicalAddress(addr.0)
    }
}

// Псевдоним для частного случая
pub type PageAlignedAddress = AlignedPhysicalAddress<12>; // 4KB страницы
