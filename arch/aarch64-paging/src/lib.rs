#![no_std]

extern crate alloc;

#[cfg(test)]
extern crate std;

pub mod entry;
pub mod level;
pub mod mapper;
pub mod mem_flags;
pub mod page_table;
pub mod preset;
pub mod table_alloc;
pub mod table_flags;
pub mod virtual_address;
