/// Virtual memory address
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct VirtualAddress(pub usize);

impl VirtualAddress {
    /// Create a new virtual address
    pub const fn new(address: usize) -> Self {
        VirtualAddress(address)
    }

    /// Get the raw address value
    pub const fn as_usize(&self) -> usize {
        self.0
    }

    /// Align up to the page boundary
    pub fn page_aligned_up(&self, page_size: usize) -> Self {
        VirtualAddress((self.0 + page_size - 1) & !(page_size - 1))
    }
}

pub(crate) trait VirtualAddressExt {
    /// Get the page offset (bits 0-11)
    fn page_offset(&self) -> usize;

    /// Get the page table indices for this address (aarch64 4-level paging)
    fn page_table_indices(&self) -> [usize; 4];
}

impl VirtualAddressExt for VirtualAddress {
    fn page_offset(&self) -> usize {
        self.0 & 0xFFF
    }

    fn page_table_indices(&self) -> [usize; 4] {
        let addr = self.0;
        [
            (addr >> 39) & 0x1FF, // Level 0 (PGD)
            (addr >> 30) & 0x1FF, // Level 1 (PUD)
            (addr >> 21) & 0x1FF, // Level 2 (PMD)
            (addr >> 12) & 0x1FF, // Level 3 (PTE)
        ]
    }
}
