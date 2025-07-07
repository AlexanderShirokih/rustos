//! Memory layout definitions
//!
//! This module defines the memory layout for the system, including
//! physical memory regions, device memory regions, and other memory-related constants.

/// Page table entry flags for aarch64
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EntryFlags(u64);

impl EntryFlags {
    // Basic flags
    pub const VALID: Self = EntryFlags(1 << 0);
    pub const TABLE: Self = EntryFlags(1 << 1);
    pub const BLOCK: Self = EntryFlags(0 << 1);
    pub const ACCESS: Self = EntryFlags(1 << 10);

    // Memory attributes (MAIR index)
    pub const NORMAL_MEMORY: Self = EntryFlags(0 << 2);
    pub const DEVICE_MEMORY: Self = EntryFlags(1 << 2);

    // Shareability
    pub const NON_SHAREABLE: Self = EntryFlags(0 << 8);
    pub const OUTER_SHAREABLE: Self = EntryFlags(2 << 8);
    pub const INNER_SHAREABLE: Self = EntryFlags(3 << 8);

    // Access permissions (AP bits)
    pub const KERNEL_RW: Self = EntryFlags(0 << 6);
    pub const KERNEL_RO: Self = EntryFlags(2 << 6);
    pub const USER_RW: Self = EntryFlags(1 << 6);
    pub const USER_RO: Self = EntryFlags(3 << 6);

    // Execute permissions
    pub const EXECUTE_NEVER: Self = EntryFlags(1 << 54);
    pub const PRIVILEGED_EXECUTE_NEVER: Self = EntryFlags(1 << 53);

    // Common combinations
    pub const KERNEL_CODE: Self = Self::combine(&[
        Self::VALID,
        Self::BLOCK,
        Self::ACCESS,
        Self::NORMAL_MEMORY,
        Self::INNER_SHAREABLE,
        Self::KERNEL_RO,
    ]);

    pub const KERNEL_DATA: Self = Self::combine(&[
        Self::VALID,
        Self::BLOCK,
        Self::ACCESS,
        Self::NORMAL_MEMORY,
        Self::INNER_SHAREABLE,
        Self::KERNEL_RW,
        Self::EXECUTE_NEVER,
    ]);

    pub const USER_CODE: Self = Self::combine(&[
        Self::VALID,
        Self::BLOCK,
        Self::ACCESS,
        Self::NORMAL_MEMORY,
        Self::INNER_SHAREABLE,
        Self::USER_RO,
        Self::PRIVILEGED_EXECUTE_NEVER,
    ]);

    pub const USER_DATA: Self = Self::combine(&[
        Self::VALID,
        Self::BLOCK,
        Self::ACCESS,
        Self::NORMAL_MEMORY,
        Self::INNER_SHAREABLE,
        Self::USER_RW,
        Self::EXECUTE_NEVER,
        Self::PRIVILEGED_EXECUTE_NEVER,
    ]);

    pub const DEVICE: Self = Self::combine(&[
        Self::VALID,
        Self::BLOCK,
        Self::ACCESS,
        Self::DEVICE_MEMORY,
        Self::OUTER_SHAREABLE,
        Self::KERNEL_RW,
        Self::EXECUTE_NEVER,
        Self::PRIVILEGED_EXECUTE_NEVER,
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
    pub const fn bits(&self) -> u64 {
        self.0
    }
}
