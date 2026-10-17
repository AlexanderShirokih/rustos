//! Раскладка виртуального адресного пространства ядра: база верхней половины, окна кучи и
//! MMIO с их предельными размерами.

use core::num::NonZeroUsize;

use memory::virtual_address::PageAlignedVirtualAddress;

pub(crate) const HIGHER_HALF_BASE: usize = 0xFFFF_FF80_0000_0000;

pub(crate) const KHEAP_BASE: PageAlignedVirtualAddress =
    PageAlignedVirtualAddress::from_usize(0xFFFF_FFF8_0000_0000).unwrap();

pub(crate) const LINEAR_MAP_SIZE: usize = KHEAP_BASE.as_usize() - HIGHER_HALF_BASE;

pub(crate) const KHEAP_MAX_SIZE: usize = 16 * 1024 * 1024 * 1024;

pub(crate) const KMMIO_BASE: PageAlignedVirtualAddress =
    PageAlignedVirtualAddress::from_usize(0xFFFF_FFFC_0000_0000).unwrap();

pub(crate) const KMMIO_MAX_SIZE: NonZeroUsize = NonZeroUsize::new(4 * 1024 * 1024 * 1024).unwrap();

pub(crate) const CNTKCTL_EL1_USER_COUNTER: u64 = 1 << 1;
