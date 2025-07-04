//! Virtual memory management
//!
//! This module provides functionality for managing virtual memory,
//! including page tables and address translation for aarch64.

use crate::kernel::memory::physical::{
    Frame, FrameAllocator, PhysicalAddress, PhysicalMemoryManager,
};
use core::fmt;
use core::ops::{Index, IndexMut};

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

    /// Align the address down to the page boundary
    pub fn align_down(&self, page_size: usize) -> VirtualAddress {
        VirtualAddress(self.0 & !(page_size - 1))
    }

    /// Align the address up to the page boundary
    pub fn align_up(&self, page_size: usize) -> VirtualAddress {
        let aligned = (self.0 + page_size - 1) & !(page_size - 1);
        VirtualAddress(aligned)
    }

    /// Get the page table indices for this address (aarch64 4-level paging)
    pub fn page_table_indices(&self) -> [usize; 4] {
        let addr = self.0;
        [
            (addr >> 39) & 0x1FF, // Level 0 (top level)
            (addr >> 30) & 0x1FF, // Level 1
            (addr >> 21) & 0x1FF, // Level 2
            (addr >> 12) & 0x1FF, // Level 3 (bottom level)
        ]
    }

    /// Get the page offset for this address
    pub fn page_offset(&self, page_size: usize) -> usize {
        self.0 & (page_size - 1)
    }
}

impl fmt::Display for VirtualAddress {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "VirtAddr(0x{:x})", self.0)
    }
}

/// A virtual memory page
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Page {
    number: usize,
}

impl Page {
    /// Create a new page from a page number
    pub const fn new(number: usize) -> Self {
        Page { number }
    }

    /// Create a page containing the given virtual address
    pub fn containing_address(address: VirtualAddress, page_size: usize) -> Self {
        Page {
            number: address.as_usize() / page_size,
        }
    }

    /// Get the starting virtual address of this page
    pub fn start_address(&self, page_size: usize) -> VirtualAddress {
        VirtualAddress(self.number * page_size)
    }

    /// Get the page number
    pub const fn number(&self) -> usize {
        self.number
    }
}

/// Page table entry flags for aarch64
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EntryFlags(u64);

impl EntryFlags {
    // Basic flags
    pub const VALID: Self = EntryFlags(1 << 0);
    pub const TABLE: Self = EntryFlags(1 << 1);
    pub const BLOCK: Self = EntryFlags(0 << 1);
    pub const ACCESS: Self = EntryFlags(1 << 10);

    // Memory attributes
    pub const NORMAL_MEMORY: Self = EntryFlags(0b11 << 2); // Inner and Outer Shareable
    pub const DEVICE_MEMORY: Self = EntryFlags(0b01 << 2); // Device-nGnRE

    // Access permissions
    pub const READ_ONLY: Self = EntryFlags(1 << 7);
    pub const READ_WRITE: Self = EntryFlags(0 << 7);
    pub const USER_ACCESSIBLE: Self = EntryFlags(1 << 6);
    pub const KERNEL_ONLY: Self = EntryFlags(0 << 6);

    // Common combinations
    pub const KERNEL_RW: Self = Self::combine(&[
        Self::VALID,
        Self::BLOCK,
        Self::ACCESS,
        Self::NORMAL_MEMORY,
        Self::READ_WRITE,
        Self::KERNEL_ONLY,
    ]);

    pub const KERNEL_RO: Self = Self::combine(&[
        Self::VALID,
        Self::BLOCK,
        Self::ACCESS,
        Self::NORMAL_MEMORY,
        Self::READ_ONLY,
        Self::KERNEL_ONLY,
    ]);

    pub const USER_RW: Self = Self::combine(&[
        Self::VALID,
        Self::BLOCK,
        Self::ACCESS,
        Self::NORMAL_MEMORY,
        Self::READ_WRITE,
        Self::USER_ACCESSIBLE,
    ]);

    pub const USER_RO: Self = Self::combine(&[
        Self::VALID,
        Self::BLOCK,
        Self::ACCESS,
        Self::NORMAL_MEMORY,
        Self::READ_ONLY,
        Self::USER_ACCESSIBLE,
    ]);

    pub const DEVICE: Self = Self::combine(&[
        Self::VALID,
        Self::BLOCK,
        Self::ACCESS,
        Self::DEVICE_MEMORY,
        Self::READ_WRITE,
        Self::KERNEL_ONLY,
    ]);

    /// Combine multiple flags
    pub const fn combine(flags: &[Self]) -> Self {
        let mut result = 0;
        let mut i = 0;
        while i < flags.len() {
            result |= flags[i].0;
            i += 1;
        }
        EntryFlags(result)
    }

    /// Check if a flag is set
    pub fn contains(&self, flag: Self) -> bool {
        (self.0 & flag.0) == flag.0
    }

    /// Add a flag
    pub fn insert(&mut self, flag: Self) {
        self.0 |= flag.0;
    }

    /// Remove a flag
    pub fn remove(&mut self, flag: Self) {
        self.0 &= !flag.0;
    }

    /// Get the raw value
    pub fn bits(&self) -> u64 {
        self.0
    }
}

/// Page table entry for aarch64
#[derive(Debug, Clone, Copy)]
#[repr(transparent)]
pub struct PageTableEntry(u64);

impl PageTableEntry {
    /// Create a new empty page table entry
    pub const fn new() -> Self {
        PageTableEntry(0)
    }

    /// Create a page table entry pointing to a physical frame with flags
    pub fn new_frame(frame: Frame, flags: EntryFlags, frame_size: usize) -> Self {
        let addr = frame.start_address(frame_size).as_usize() as u64;
        PageTableEntry((addr & 0x0000_FFFF_FFFF_F000) | flags.bits())
    }

    /// Create a page table entry pointing to another page table
    pub fn new_table(table_frame: Frame, frame_size: usize) -> Self {
        let addr = table_frame.start_address(frame_size).as_usize() as u64;
        let flags = EntryFlags::combine(&[EntryFlags::VALID, EntryFlags::TABLE]);
        PageTableEntry((addr & 0x0000_FFFF_FFFF_F000) | flags.bits())
    }

    /// Check if the entry is valid (present)
    pub fn is_valid(&self) -> bool {
        (self.0 & 1) == 1
    }

    /// Check if the entry points to a page table
    pub fn is_table(&self) -> bool {
        self.is_valid() && (self.0 & 2) == 2
    }

    /// Check if the entry points to a block/page
    pub fn is_block(&self) -> bool {
        self.is_valid() && (self.0 & 2) == 0
    }

    /// Get the physical address this entry points to
    pub fn address(&self) -> PhysicalAddress {
        PhysicalAddress::new((self.0 & 0x0000_FFFF_FFFF_F000) as usize)
    }

    /// Get the frame this entry points to
    pub fn frame(&self, frame_size: usize) -> Frame {
        Frame::containing_address(self.address(), frame_size)
    }

    /// Get the flags for this entry
    pub fn flags(&self) -> EntryFlags {
        EntryFlags(self.0 & 0xFFF)
    }

    /// Set the entry to a new value
    pub fn set(&mut self, entry: PageTableEntry) {
        self.0 = entry.0;
    }
}

/// A page table (512 entries for aarch64)
#[repr(align(4096))]
pub struct PageTable {
    entries: [PageTableEntry; 512],
}

impl PageTable {
    /// Create a new empty page table
    pub fn new() -> Self {
        PageTable {
            entries: [PageTableEntry::new(); 512],
        }
    }

    /// Clear all entries in the page table
    pub fn clear(&mut self) {
        for entry in &mut self.entries {
            *entry = PageTableEntry::new();
        }
    }
}

impl Index<usize> for PageTable {
    type Output = PageTableEntry;

    fn index(&self, index: usize) -> &Self::Output {
        &self.entries[index]
    }
}

impl IndexMut<usize> for PageTable {
    fn index_mut(&mut self, index: usize) -> &mut Self::Output {
        &mut self.entries[index]
    }
}

/// Page table manager for aarch64
pub struct PageTableManager {
    frame_size: usize,

    /// Root page table (level 0)
    root_table: Frame,
}

impl PageTableManager {
    pub fn frame_size(&self) -> usize {
        self.frame_size
    }

    /// Create a new page table manager with a new root table
    pub fn new(frame_allocator: &mut impl FrameAllocator) -> Self {
        let root_frame = frame_allocator
            .allocate_frame()
            .expect("Failed to allocate frame for root page table");

        // Clear the root table
        let root_table = unsafe {
            &mut *(root_frame
                .start_address(frame_allocator.frame_size())
                .as_usize() as *mut PageTable)
        };
        root_table.clear();

        PageTableManager {
            frame_size: frame_allocator.frame_size(),
            root_table: root_frame,
        }
    }

    /// Map a virtual page to a physical frame
    pub fn map(
        &mut self,
        page: Page,
        frame: Frame,
        flags: EntryFlags,
        frame_allocator: &mut impl FrameAllocator,
    ) -> Result<(), &'static str> {
        let indices = page.start_address(self.frame_size).page_table_indices();

        // Get or create the page tables for each level
        let mut table_frame = self.root_table;

        // Navigate through the page table levels
        for level in 0..3 {
            let table = unsafe {
                &mut *(table_frame.start_address(self.frame_size).as_usize() as *mut PageTable)
            };
            let idx = indices[level];

            if !table[idx].is_valid() {
                // Need to create a new table for this level
                let new_table_frame = frame_allocator
                    .allocate_frame()
                    .ok_or("Failed to allocate frame for page table")?;

                // Clear the new table
                let new_table = unsafe {
                    &mut *(new_table_frame.start_address(self.frame_size).as_usize()
                        as *mut PageTable)
                };
                new_table.clear();

                // Link it from the parent table
                table[idx].set(PageTableEntry::new_table(new_table_frame, self.frame_size));
                table_frame = new_table_frame;
            } else if table[idx].is_table() {
                // Follow the existing table
                table_frame = table[idx].frame(self.frame_size);
            } else {
                // Entry is a block mapping, which is invalid for levels 0-2
                return Err("Cannot map page: entry is already a block mapping");
            }
        }

        // Now we're at the level 3 (leaf) page table
        let leaf_table = unsafe {
            &mut *(table_frame.start_address(self.frame_size).as_usize() as *mut PageTable)
        };
        let idx = indices[3];

        if leaf_table[idx].is_valid() {
            return Err("Page is already mapped");
        }

        // Create the final mapping
        leaf_table[idx].set(PageTableEntry::new_frame(frame, flags, self.frame_size));

        Ok(())
    }

    /// Unmap a virtual page
    pub fn unmap(&mut self, page: Page) -> Result<Frame, &'static str> {
        let indices = page.start_address(self.frame_size).page_table_indices();

        // Navigate to the leaf page table
        let mut table_frame = self.root_table;
        let mut table = unsafe {
            &mut *(table_frame.start_address(self.frame_size).as_usize() as *mut PageTable)
        };

        // Check each level
        for level in 0..3 {
            let idx = indices[level];
            if !table[idx].is_valid() || !table[idx].is_table() {
                return Err("Page is not mapped");
            }
            table_frame = table[idx].frame(self.frame_size);
            table = unsafe {
                &mut *(table_frame.start_address(self.frame_size).as_usize() as *mut PageTable)
            };
        }

        // At the leaf level
        let idx = indices[3];
        if !table[idx].is_valid() || !table[idx].is_block() {
            return Err("Page is not mapped");
        }

        // Get the frame before unmapping
        let frame = table[idx].frame(self.frame_size);

        // Clear the entry
        table[idx].set(PageTableEntry::new());

        Ok(frame)
    }

    /// Translate a virtual address to a physical address
    pub fn translate(&self, addr: VirtualAddress) -> Option<PhysicalAddress> {
        let indices = addr.page_table_indices();

        // Start at the root table
        let mut table_frame = self.root_table;
        let mut table =
            unsafe { &*(table_frame.start_address(4096).as_usize() as *const PageTable) };

        // Navigate through the page table levels
        for level in 0..4 {
            let idx = indices[level];
            let entry = table[idx];

            if !entry.is_valid() {
                return None; // Not mapped
            }

            if level == 3 || (entry.is_block() && level > 0) {
                // This is a leaf entry (either at level 3 or a block entry at levels 1-2)
                // Calculate the physical address
                let frame_addr = entry.address().as_usize();

                let page_mask = if level == 1 {
                    // 1GB page
                    0x3FFF_FFFF
                } else if level == 2 {
                    // 2MB page
                    0x1F_FFFF
                } else {
                    // 4KB page
                    0xFFF
                };

                let addr_offset = addr.as_usize() & page_mask;
                return Some(PhysicalAddress::new(frame_addr + addr_offset));
            }

            // Move to the next level
            table_frame = entry.frame(self.frame_size);
            table = unsafe {
                &*(table_frame.start_address(self.frame_size).as_usize() as *const PageTable)
            };
        }

        None // Should not reach here
    }

    /// Get the root page table frame
    pub fn root_frame(&self) -> Frame {
        self.root_table
    }

    /// Set the TTBRx register to use this page table
    pub fn activate(&self) {
        let ttbr0_val = self.root_table.start_address(self.frame_size).as_usize() as u64;

        unsafe {
            // Set TTBR0_EL1 (used for user space)
            core::arch::asm!(
            "msr ttbr0_el1, {0}",
            "isb",
            in(reg) ttbr0_val
            );

            // Flush the TLB
            core::arch::asm!("tlbi vmalle1", "dsb sy", "isb");
        }
    }
}

/// Initialize the virtual memory system
pub fn init(physical_memory_manager: &mut PhysicalMemoryManager) -> PageTableManager {
    // Create a new page table manager
    let mut page_table_manager = PageTableManager::new(physical_memory_manager);
    let page_size = page_table_manager.frame_size();

    // Identity-map the first 128MB of physical memory
    // This ensures the kernel and essential memory regions remain accessible
    for addr in (0..0x8000_0000).step_by(page_size) {
        let page = Page::containing_address(VirtualAddress::new(addr), page_size);
        let frame = Frame::containing_address(PhysicalAddress::new(addr), page_size);

        // Use different flags based on the memory region
        let flags = if addr < 0x4000_0000 {
            // First 64MB: Device memory (for MMIO)
            EntryFlags::DEVICE
        } else if addr < 0x4020_0000 {
            // Kernel stack: Read-write
            EntryFlags::KERNEL_RW
        } else if addr < 0x4040_0000 {
            // Kernel code: Read-only
            EntryFlags::KERNEL_RO
        } else {
            // Remaining memory: Read-write
            EntryFlags::KERNEL_RW
        };

        // Ignore errors - some pages might already be mapped
        let _ = page_table_manager.map(page, frame, flags, physical_memory_manager);
    }

    page_table_manager
}
