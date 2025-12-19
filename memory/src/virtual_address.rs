use crate::aligned::{Address, Aligned};
use crate::physical_address::{AlignedPhysicalAddress, PhysicalAddress};

/// Адрес виртуальной памяти
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[repr(transparent)]
pub struct VirtualAddress(usize);

impl VirtualAddress {
    pub const fn new(address: usize) -> Self {
        VirtualAddress(address)
    }

    pub const fn add(&self, offset: usize) -> Self {
        VirtualAddress::new(self.0 + offset)
    }
}

impl Address for VirtualAddress {
    fn as_usize(self) -> usize {
        self.0
    }

    fn as_physical_address(self) -> PhysicalAddress {
        PhysicalAddress::new(self.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[repr(transparent)]
pub struct AlignedVirtualAddress<const SHIFT: u8>(VirtualAddress);

impl<const SHIFT: u8> AlignedVirtualAddress<SHIFT> {
    pub fn new(address: VirtualAddress) -> Option<Self> {
        if address.0 % Self::ALIGNMENT == 0 {
            Some(Self(address))
        } else {
            None
        }
    }

    pub fn new_unchecked(address: VirtualAddress) -> Self {
        Self(address)
    }

    pub fn as_usize(&self) -> usize {
        self.0.as_usize()
    }

    pub fn from_usize(address: usize) -> Option<Self> {
        if address % Self::ALIGNMENT == 0 {
            Some(Self(VirtualAddress(address)))
        } else {
            None
        }
    }

    pub const fn identity(address: &AlignedPhysicalAddress<SHIFT>) -> Self {
        Self(VirtualAddress(address.as_usize()))
    }

    pub const fn next_aligned(&self) -> Self {
        Self(self.0.add(Self::ALIGNMENT))
    }
}

impl<const SHIFT: u8> Aligned for AlignedVirtualAddress<SHIFT> {
    const ALIGNMENT: usize = 1usize << SHIFT;
}

impl<const SHIFT: u8> Into<VirtualAddress> for AlignedVirtualAddress<SHIFT> {
    fn into(self) -> VirtualAddress {
        self.0
    }
}

pub type PageAlignedVirtualAddress = AlignedVirtualAddress<12>;
