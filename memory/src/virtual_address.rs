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

    /// Пишет значение `T` в этот адрес.
    ///
    /// # Panics (debug)
    /// если адрес не выровнен.
    ///
    /// # Safety-internal
    /// Предусловия: адрес валиден, принадлежит heap-региону, доступен на запись,
    /// и память в этом месте предназначена под `T`.
    pub unsafe fn write<T>(self, value: T) {
        debug_assert_eq!(self.0 % align_of::<T>(), 0);
        unsafe { core::ptr::write(self.0 as *mut T, value) }
    }

    pub fn as_ptr<T>(&self) -> *mut T {
        self.0 as *mut T
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

    pub const fn as_usize(&self) -> usize {
        self.0.0
    }

    pub const fn as_ptr<T>(&self) -> *mut T {
        self.0.0 as *mut T
    }

    pub const fn from_usize(address: usize) -> Option<Self> {
        if address % Self::ALIGNMENT == 0 {
            Some(Self(VirtualAddress(address)))
        } else {
            None
        }
    }

    pub const fn identity(address: AlignedPhysicalAddress<SHIFT>) -> Self {
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
