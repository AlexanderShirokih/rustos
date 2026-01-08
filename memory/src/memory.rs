use crate::virtual_address::AlignedVirtualAddress;

pub trait MemoryAccessProvider {
    fn clean_cache<const SHIFT: u8>(&self, address: AlignedVirtualAddress<SHIFT>);
    fn invalidate_cache(&self);
}
