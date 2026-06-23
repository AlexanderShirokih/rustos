//! Крейт управления памятью ядра.
//!
//! Предоставляет примитивы для работы с физической и виртуальной памятью:
//! аллокаторы фреймов, bump-аллокатор, аллокатор кучи и типы адресов.

#![cfg_attr(not(test), no_std)]
extern crate alloc;

#[cfg(test)]
extern crate std;

pub mod align;
pub mod aligned;
pub mod bump_allocator;
pub mod frame;
pub mod frame_allocator;
mod frame_bitmap;
pub mod kernel_vm_allocator;
pub mod mem_flags;
pub mod memory;
pub mod memory_mapper;
pub mod memory_range;
pub mod physical_address;
pub mod range_allocator;
pub mod region;
pub mod relocatable_ptr;
pub mod user_vm_allocator;
pub mod user_vm_context;
pub mod virtual_address;

#[cfg_attr(not(test), doc(hidden))]
pub use frame_bitmap::FrameBitmap;
pub use mem_flags::MemFlags;
pub use region::{AccessMask, BudgetRefund, MemoryBacking, MemoryRegion, RegionCreateError};
pub use relocatable_ptr::RelocatablePtr;
pub use user_vm_allocator::{MappingTag, UserVmAllocator};
pub use user_vm_context::{UserVmContext, WeakUserVmContext};
