#![cfg_attr(not(test), no_std)]
extern crate alloc;

#[cfg(test)]
extern crate std;

pub mod aligned;
pub mod bump_allocator;
pub mod frame;
pub mod frame_allocator;
mod frame_bitmap;
pub mod heap_allocator;
pub mod memory;
pub mod memory_mapper;
pub mod memory_range;
pub mod physical_address;
pub mod region_manager;
pub mod test_utils;
pub mod virtual_address;

#[cfg_attr(not(test), doc(hidden))]
pub use frame_bitmap::FrameBitmap;
