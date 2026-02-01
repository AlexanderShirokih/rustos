use crate::aligned::{Address, Aligned};
use crate::physical_address::AlignedPhysicalAddress;
use core::fmt::{Formatter, LowerHex};

/// Адрес виртуальной памяти
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[repr(transparent)]
pub struct VirtualAddress(usize);

impl VirtualAddress {
    pub const fn new(address: usize) -> Self {
        VirtualAddress(address)
    }

    pub const fn offset(self, offset: usize) -> Self {
        VirtualAddress::new(self.0 + offset)
    }

    pub const fn as_usize(self) -> usize {
        self.0
    }

    pub fn as_ptr<T>(&self) -> *mut T {
        self.0 as *mut T
    }
}

impl Address for VirtualAddress {
    fn as_usize(self) -> usize {
        self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[repr(transparent)]
pub struct AlignedVirtualAddress<const SHIFT: u8>(VirtualAddress);

impl<const SHIFT: u8> AlignedVirtualAddress<SHIFT> {
    pub const fn new(address: VirtualAddress) -> Option<Self> {
        if address.0.is_multiple_of(Self::ALIGNMENT) {
            Some(Self(address))
        } else {
            None
        }
    }

    pub fn new_unchecked(address: VirtualAddress) -> Self {
        Self(address)
    }

    pub const fn as_usize(&self) -> usize {
        self.as_virtual().as_usize()
    }

    pub const fn as_ptr<T>(&self) -> *mut T {
        self.0.0 as *mut T
    }

    pub const fn as_virtual(&self) -> VirtualAddress {
        self.0
    }

    pub const fn from_usize(address: usize) -> Option<Self> {
        if address.is_multiple_of(Self::ALIGNMENT) {
            Some(Self(VirtualAddress(address)))
        } else {
            None
        }
    }

    pub const fn identity(address: AlignedPhysicalAddress<SHIFT>) -> Self {
        Self(VirtualAddress(address.as_usize()))
    }

    pub const fn next_aligned(&self) -> Self {
        Self(self.0.offset(Self::ALIGNMENT))
    }

    pub const fn offset(&self, bytes: usize) -> Option<Self> {
        Self::new(VirtualAddress::new(self.as_usize() + bytes))
    }
}

impl<const SHIFT: u8> Aligned for AlignedVirtualAddress<SHIFT> {
    const ALIGNMENT: usize = 1usize << SHIFT;
}

impl<const SHIFT: u8> From<AlignedVirtualAddress<SHIFT>> for VirtualAddress {
    fn from(val: AlignedVirtualAddress<SHIFT>) -> Self {
        val.0
    }
}

impl<const SHIFT: u8> LowerHex for AlignedVirtualAddress<SHIFT> {
    fn fmt(&self, f: &mut Formatter<'_>) -> core::fmt::Result {
        LowerHex::fmt(&self.as_usize(), f)
    }
}

pub type PageAlignedVirtualAddress = AlignedVirtualAddress<12>;
