//! Virtual memory management
//!
//! This module provides functionality for managing virtual memory,
//! including page tables and address translation for aarch64.

use crate::memory::entry_flags::EntryFlags;
use crate::memory::layout::{MemoryLayout, MemoryRegion};
use crate::memory::virtual_address::{VirtualAddress, VirtualAddressExt};
use core::ops::{Index, IndexMut};
use kernel_core::log::{debug, info};
use memory::memory_backend::{MemoryBackend, MemoryBackendExt, MemoryPtr};
use memory::physical::{Frame, FrameAllocator, PhysicalAddress};
use util::string::usize_to_str;

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

    /// Get the next page
    pub fn next(&self) -> Self {
        Page::new(self.number + 1)
    }

    /// Create a range of pages
    pub fn range_inclusive(start: Page, end: Page) -> impl Iterator<Item = Page> {
        (start.number..=end.number).map(Page::new)
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
        // Ensure address is properly aligned and within valid range
        let masked_addr = addr & 0x0000_FFFF_FFFF_F000;
        PageTableEntry(masked_addr | flags.bits())
    }

    /// Create a page table entry pointing to another page table
    pub fn new_table(table_frame: Frame, frame_size: usize) -> Self {
        let addr = table_frame.start_address(frame_size).as_usize() as u64;
        let masked_addr = addr & 0x0000_FFFF_FFFF_F000;
        let flags = EntryFlags::combine(&[EntryFlags::VALID, EntryFlags::TABLE]);
        PageTableEntry(masked_addr | flags.bits())
    }

    /// Check if the entry is valid (present)
    pub fn is_valid(&self) -> bool {
        (self.0 & EntryFlags::VALID.bits()) != 0
    }

    /// Check if the entry points to a page table
    pub fn is_table(&self) -> bool {
        self.is_valid() && (self.0 & EntryFlags::TABLE.bits()) != 0
    }

    /// Check if the entry points to a block/page
    pub fn is_block(&self) -> bool {
        self.is_valid() && (self.0 & EntryFlags::TABLE.bits()) == 0
    }

    /// Get the physical address this entry points to
    pub fn address(&self) -> PhysicalAddress {
        PhysicalAddress::new((self.0 & 0x0000_FFFF_FFFF_F000) as usize)
    }

    /// Get the frame this entry points to
    pub fn frame(&self, frame_size: usize) -> Frame {
        Frame::containing_address(self.address(), frame_size)
    }

    /// Set the entry to a new value
    pub fn set(&mut self, entry: PageTableEntry) {
        self.0 = entry.0;
    }

    /// Clear the entry
    pub fn clear(&mut self) {
        self.0 = 0;
    }
}

/// A page table (512 entries for aarch64)
#[repr(align(4096))]
#[derive(Copy, Clone)]
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
            entry.clear();
        }
    }

    /// Get an iterator over all entries
    pub fn iter(&self) -> impl Iterator<Item = &PageTableEntry> {
        self.entries.iter()
    }

    /// Get a mutable iterator over all entries
    pub fn iter_mut(&mut self) -> impl Iterator<Item = &mut PageTableEntry> {
        self.entries.iter_mut()
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

/// Error types for virtual memory operations
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VmError {
    /// Page is already mapped
    AlreadyMapped,
    /// Page is not mapped
    NotMapped,
    /// Cannot map page due to existing block mapping
    BlockMappingExists,
    /// Frame allocation failed
    FrameAllocationFailed,
    /// Invalid table entry found
    InvalidTableEntry,
    /// Invalid page table level
    InvalidLevel,
}

impl From<&'static str> for VmError {
    fn from(s: &'static str) -> Self {
        match s {
            "Page is already mapped" => VmError::AlreadyMapped,
            "Page is not mapped" => VmError::NotMapped,
            "Cannot map page: entry is already a block mapping" => VmError::BlockMappingExists,
            "Failed to allocate frame for page table" => VmError::FrameAllocationFailed,
            _ => VmError::InvalidTableEntry,
        }
    }
}

/// Page table manager for aarch64
pub struct PageTableManager<B: MemoryBackend + 'static> {
    frame_size: usize,
    /// Root page table (level 0)
    root_table: Frame,

    /// Backend for memory operations
    backend: &'static B,

    frame_allocator: &'static dyn FrameAllocator,
    /// Heap memory region
    pub heap: MemoryRegion,
}

impl<B: MemoryBackend + 'static> PageTableManager<B> {
    /// Get the frame size
    pub fn frame_size(&self) -> usize {
        self.frame_size
    }

    /// Create a new page table manager with a new root table
    pub fn new(
        frame_allocator: &'static dyn FrameAllocator,
        backend: &'static B,
        heap: MemoryRegion,
    ) -> Result<Self, VmError> {
        let root_frame = frame_allocator
            .allocate_frame()
            .ok_or(VmError::FrameAllocationFailed)?;

        debug("Allocated root page addr: ");
        debug(usize_to_str(root_frame.start_address(backend.frame_size()).as_usize()));

        let frame_address = root_frame.start_address(backend.frame_size());
        let root_ptr: MemoryPtr<PageTable> = frame_address.into();

        // Clear the root table
        let mut page_table = PageTable::new();
        page_table.clear();
        root_ptr.write(backend, page_table);

        Ok(PageTableManager {
            frame_size: frame_allocator.frame_size(),
            root_table: root_frame,
            frame_allocator,
            backend,
            heap,
        })
    }

    /// Enable or disable virtual memory mode
    pub fn enable_virtual_mode(&self) {
        self.backend
            .enable_virtual_mode(self.root_table.start_address(self.frame_size));
    }

    /// Identity map a memory region
    fn identity_map_region(
        &self,
        from: PhysicalAddress,
        to: PhysicalAddress,
        flags: EntryFlags,
    ) -> Result<(), VmError> {
        assert!(from.as_usize() < to.as_usize());

        for addr in (from.as_usize()..to.as_usize()).step_by(self.frame_size) {
            let page = Page::containing_address(VirtualAddress::new(addr), self.frame_size);
            let frame = Frame::containing_address(PhysicalAddress::new(addr), self.frame_size);
            self.map(page, frame, flags)?;
        }

        Ok(())
    }

    /// Map a virtual page to a physical frame
    pub fn map(&self, page: Page, frame: Frame, flags: EntryFlags) -> Result<(), VmError> {
        let indices = page.start_address(self.frame_size).page_table_indices();
        let mut table_frame = self.root_table;

        // Navigate through the page table levels (0-2)
        for level in 0..3 {
            let table_address = table_frame.start_address(self.frame_size);
            let mut table: PageTable = self.backend.read(table_address);
            let idx = indices[level];

            if !table[idx].is_valid() {
                // Create a new page table
                let new_table_frame = self
                    .frame_allocator
                    .allocate_frame()
                    .ok_or(VmError::FrameAllocationFailed)?;

                // Initialize the new table
                let new_table_address = new_table_frame.start_address(self.frame_size);
                let mut new_table = PageTable::new();
                new_table.clear();
                self.backend.write(new_table_address, new_table);

                // Link it from the parent table
                table[idx].set(PageTableEntry::new_table(new_table_frame, self.frame_size));
                self.backend.write(table_address, table);

                table_frame = new_table_frame;
            } else if table[idx].is_table() {
                // Follow the existing table
                table_frame = table[idx].frame(self.frame_size);
            } else {
                // Entry is a block mapping, which conflicts with our page mapping
                return Err(VmError::BlockMappingExists);
            }
        }

        // Now we're at the level 3 (leaf) page table
        let leaf_address = table_frame.start_address(self.frame_size);
        let mut leaf_table: PageTable = self.backend.read(leaf_address);
        let idx = indices[3];

        if leaf_table[idx].is_valid() {
            return Err(VmError::AlreadyMapped);
        }

        // Create the final mapping
        leaf_table[idx].set(PageTableEntry::new_frame(frame, flags, self.frame_size));
        self.backend.write(leaf_address, leaf_table);

        let phys_addr = frame.start_address(self.frame_size);
        self.backend.clean_dcache_page(phys_addr);
        self.backend.invalidate_cache();

        Ok(())
    }

    /// Unmap a virtual page
    pub fn unmap(&self, page: Page) -> Result<Frame, VmError> {
        let indices = page.start_address(self.frame_size).page_table_indices();
        let mut table_frame = self.root_table;

        // Navigate to the leaf page table
        for level in 0..3 {
            let table_address = table_frame.start_address(self.frame_size);
            let table: PageTable = self.backend.read(table_address);
            let idx = indices[level];

            if !table[idx].is_valid() || !table[idx].is_table() {
                return Err(VmError::NotMapped);
            }
            table_frame = table[idx].frame(self.frame_size);
        }

        // At the leaf level (level 3)
        let leaf_address = table_frame.start_address(self.frame_size);
        let mut leaf_table: PageTable = self.backend.read(leaf_address);
        let idx = indices[3];

        if !leaf_table[idx].is_valid() {
            return Err(VmError::NotMapped);
        }

        if leaf_table[idx].is_table() {
            return Err(VmError::InvalidTableEntry);
        }

        // Get the frame before unmapping
        let frame = leaf_table[idx].frame(self.frame_size);

        // Clear the entry
        leaf_table[idx].clear();
        self.backend.write(leaf_address, leaf_table);

        // TODO: Consider deallocating empty page tables to prevent memory leaks

        Ok(frame)
    }

    /// Map a range of pages to a range of frames
    pub fn map_range(
        &self,
        pages: impl Iterator<Item = Page>,
        frames: impl Iterator<Item = Frame>,
        flags: EntryFlags,
    ) -> Result<(), VmError> {
        for (page, frame) in pages.zip(frames) {
            self.map(page, frame, flags)?;
        }
        Ok(())
    }

    /// Translate a virtual address to a physical address
    pub fn translate(&self, addr: VirtualAddress) -> Option<PhysicalAddress> {
        let indices = addr.page_table_indices();
        let mut table_frame = self.root_table;

        // Navigate through the page table levels
        for level in 0..4 {
            let table_address = table_frame.start_address(self.frame_size);
            let table: PageTable = self.backend.read(table_address);
            let idx = indices[level];
            let entry = table[idx];

            if !entry.is_valid() {
                return None;
            }

            // Check for block mappings at levels 1 and 2
            if level > 0 && entry.is_block() {
                let block_size = match level {
                    1 => 1024 * 1024 * 1024, // 1GB
                    2 => 2 * 1024 * 1024,    // 2MB
                    _ => return None,        // Invalid
                };

                let block_mask = block_size - 1;
                let offset = addr.as_usize() & block_mask;
                return Some(entry.address().offset_bytes(offset));
            }

            // At level 3, we should have a page mapping
            if level == 3 {
                if entry.is_table() {
                    return None; // Invalid: level 3 cannot have table entries
                }
                let offset = addr.page_offset();
                return Some(entry.address().offset_bytes(offset));
            }

            // Continue to the next level
            if !entry.is_table() {
                return None; // Should be a table entry at levels 0-2
            }

            table_frame = entry.frame(self.frame_size);
        }

        None
    }

    /// Check if a virtual address is mapped
    pub fn is_mapped(&self, addr: VirtualAddress) -> bool {
        self.translate(addr).is_some()
    }

    /// Get the root page table frame
    pub fn root_frame(&self) -> Frame {
        self.root_table
    }

    /// Get memory usage statistics
    pub fn memory_stats(&self) -> MemoryStats {
        let mut stats = MemoryStats::default();
        self.collect_stats_recursive(self.root_table, 0, &mut stats);
        stats
    }

    /// Recursively collect memory usage statistics
    fn collect_stats_recursive(&self, table_frame: Frame, level: usize, stats: &mut MemoryStats) {
        if level >= 4 {
            return;
        }

        let table_address = table_frame.start_address(self.frame_size);
        let table: PageTable = self.backend.read(table_address);
        stats.page_tables += 1;

        for entry in table.iter() {
            if entry.is_valid() {
                if entry.is_table() && level < 3 {
                    self.collect_stats_recursive(entry.frame(self.frame_size), level + 1, stats);
                } else if entry.is_block() || level == 3 {
                    stats.mapped_pages += 1;
                }
            }
        }
    }
}

/// Memory usage statistics
#[derive(Debug, Default, Clone, Copy)]
pub struct MemoryStats {
    /// Number of page tables allocated
    pub page_tables: usize,
    /// Number of mapped pages
    pub mapped_pages: usize,
}

impl MemoryStats {
    /// Get the total memory used by page tables
    pub fn page_table_memory(&self, frame_size: usize) -> usize {
        self.page_tables * frame_size
    }

    /// Get the total memory used by mapped pages
    pub fn mapped_memory(&self, frame_size: usize) -> usize {
        self.mapped_pages * frame_size
    }
}

/// Initialize the virtual memory system
pub(crate) fn init<B: MemoryBackend + 'static>(
    frame_allocator: &'static dyn FrameAllocator,
    memory_backend: &'static B,
    memory_layout: MemoryLayout,
) -> Result<PageTableManager<B>, VmError> {
    let identity_map_regions = [
        memory_layout.devices,
        memory_layout.kernel,
        memory_layout.stack,
    ];

    // Reserve frames for direct-mapped regions first
    for region in &identity_map_regions {
        debug("Allocating frames for direct-mapped region: ");
        debug(region.label);

        let frame_count =
            (region.end.as_usize() - region.start.as_usize()) / frame_allocator.frame_size();
        if frame_count > 0 {
            frame_allocator
                .allocate_frames_exact(region.start, frame_count)
                .ok_or(VmError::FrameAllocationFailed)?;
        }
    }

    // Create the page table manager
    let page_table_manager =
        PageTableManager::new(frame_allocator, memory_backend, memory_layout.heap)?;

    // Identity map all direct regions
    for region in &identity_map_regions {
        page_table_manager.identity_map_region(region.start, region.end, region.flags)?;
    }

    info("Direct-mapped regions initialized");

    // Enable virtual memory
    page_table_manager.enable_virtual_mode();

    info("Virtual memory enabled");

    Ok(page_table_manager)
}
