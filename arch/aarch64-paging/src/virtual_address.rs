use crate::level::Level;
use memory::virtual_address::AlignedVirtualAddress;

pub trait VirtualAddressExt {
    fn index<L: Level>(self) -> usize;
}

impl<const SHIFT: u8> VirtualAddressExt for AlignedVirtualAddress<SHIFT> {
    fn index<L: Level>(self) -> usize {
        (self.as_usize() >> L::SHIFT) & 0x1FF
    }
}
