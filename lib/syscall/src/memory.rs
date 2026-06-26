//! Маска доступа региона памяти в syscall ABI.

/// Маска доступа региона, кодирующая разрешённые операции теми же битами,
/// что ждёт ядро (`access_raw & 0b111`): нулевая маска отвергается как
/// `InvalidArgument`. Передаётся в memory_create_virtual/memory_slice.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(transparent)]
pub struct MemoryAccess(u64);

impl MemoryAccess {
    pub const READ: Self = Self(1);
    pub const WRITE: Self = Self(2);
    pub const EXECUTE: Self = Self(4);
    pub const RW: Self = Self(Self::READ.0 | Self::WRITE.0);
    pub const RWX: Self = Self(Self::READ.0 | Self::WRITE.0 | Self::EXECUTE.0);

    const ALL_BITS: u64 = Self::READ.0 | Self::WRITE.0 | Self::EXECUTE.0;

    /// Принимает произвольный набор битов, отбрасывая позиции вне R/W/X.
    pub const fn from_bits_truncate(bits: u64) -> Self {
        Self(bits & Self::ALL_BITS)
    }

    /// Wire-кодировка `access_mask` для memory_create_virtual/memory_slice.
    pub const fn raw(self) -> u64 {
        self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derived_masks_are_unions() {
        assert_eq!(
            MemoryAccess::RW.raw(),
            MemoryAccess::READ.raw() | MemoryAccess::WRITE.raw()
        );
        assert_eq!(
            MemoryAccess::RWX.raw(),
            MemoryAccess::READ.raw() | MemoryAccess::WRITE.raw() | MemoryAccess::EXECUTE.raw()
        );
    }

    #[test]
    fn from_bits_truncate_drops_high_bits() {
        assert_eq!(MemoryAccess::from_bits_truncate(0xFF), MemoryAccess::RWX);
        assert_eq!(MemoryAccess::from_bits_truncate(0b11), MemoryAccess::RW);
    }
}
