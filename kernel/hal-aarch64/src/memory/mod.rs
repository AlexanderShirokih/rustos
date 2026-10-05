//! Управление памятью AArch64.

pub(crate) mod address_space_factory;
pub(crate) mod asid;
pub(crate) mod global_allocator;
pub(crate) mod layout;
pub(crate) mod memory_mapper;
pub(crate) mod memory_setup;
pub(crate) mod mmu;
pub(crate) mod regs;
pub(crate) mod setup;
