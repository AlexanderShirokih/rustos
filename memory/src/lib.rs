#![cfg_attr(not(test), no_std)]
extern crate alloc;

#[cfg(test)]
extern crate std;

pub mod memory_backend;
pub mod memory_range;
pub mod physical;
pub mod physical_manager;

mod frame_bitmap;

#[cfg_attr(not(test), doc(hidden))]
pub use frame_bitmap::{FrameBitmap, FrameBitmapError};
