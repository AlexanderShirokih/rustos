#![no_std]

extern crate alloc;

#[cfg(test)]
extern crate std;

pub mod entry_flags;
pub mod layout;
pub mod virtual_address;
pub mod virtual_mem;

pub use entry_flags::EntryFlags;
pub use layout::{MemoryLayout, MemoryRegion};
pub use virtual_address::{VirtualAddress, VirtualAddressExt};
pub use virtual_mem::{
    Page, PageTable, PageTableEntry, PageTableManager, VmError, create_page_table_manager,
};
