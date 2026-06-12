//! Управление памятью AArch64.

pub mod address_space_factory;
pub mod asid;
pub mod global_allocator;
pub mod layout;
pub mod memory_mapper;
pub mod memory_setup;
pub mod mmu;
pub mod regs;
pub(crate) mod setup;
