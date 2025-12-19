use crate::virtual_address::{AlignedVirtualAddress, VirtualAddress};

pub trait MemoryAccessProvider {
    fn read<T>(&self, addr: VirtualAddress) -> T;
    fn write<T>(&self, addr: VirtualAddress, val: &T);

    fn clean_cache<const SHIFT: u8>(&self, address: AlignedVirtualAddress<SHIFT>);
    fn invalidate_cache(&self);
}
