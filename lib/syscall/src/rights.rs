use core::ops::{BitAnd, BitAndAssign, BitOr, BitOrAssign, Not};

/// Набор прав, ассоциированных с конкретным capability.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
#[repr(transparent)]
pub struct Rights(u32);

impl Rights {
    pub const NONE: Self = Self(0);
    pub const DUPLICATE: Self = Self(1 << 0);
    pub const TRANSFER: Self = Self(1 << 1);
    pub const READ: Self = Self(1 << 2);
    pub const WRITE: Self = Self(1 << 3);
    pub const EXECUTE: Self = Self(1 << 4);

    const ALL_BITS: u32 =
        Self::DUPLICATE.0 | Self::TRANSFER.0 | Self::READ.0 | Self::WRITE.0 | Self::EXECUTE.0;

    pub const fn empty() -> Self {
        Self(0)
    }

    pub const fn all() -> Self {
        Self(Self::ALL_BITS)
    }

    pub const fn bits(self) -> u32 {
        self.0
    }

    /// Принимает произвольный набор битов, отбрасывая неизвестные позиции.
    pub const fn from_bits_truncate(bits: u32) -> Self {
        Self(bits & Self::ALL_BITS)
    }

    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// `true`, если `self` содержит все биты из `other`.
    pub const fn contains(self, other: Self) -> bool {
        (self.0 & other.0) == other.0
    }

    /// `true`, если все биты `self` уже присутствуют в `other`.
    pub const fn is_subset_of(self, other: Self) -> bool {
        (self.0 & other.0) == self.0
    }
}

impl BitOr for Rights {
    type Output = Self;
    fn bitor(self, rhs: Self) -> Self {
        Self(self.0 | rhs.0)
    }
}

impl BitOrAssign for Rights {
    fn bitor_assign(&mut self, rhs: Self) {
        self.0 |= rhs.0;
    }
}

impl BitAnd for Rights {
    type Output = Self;
    fn bitand(self, rhs: Self) -> Self {
        Self(self.0 & rhs.0)
    }
}

impl BitAndAssign for Rights {
    fn bitand_assign(&mut self, rhs: Self) {
        self.0 &= rhs.0;
    }
}

impl Not for Rights {
    type Output = Self;
    fn not(self) -> Self {
        Self(!self.0 & Self::ALL_BITS)
    }
}

impl From<Rights> for u32 {
    fn from(rights: Rights) -> Self {
        rights.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bitor_is_union() {
        let r = Rights::READ | Rights::WRITE;
        assert!(r.contains(Rights::READ));
        assert!(r.contains(Rights::WRITE));
        assert!(!r.contains(Rights::EXECUTE));
    }

    #[test]
    fn bitand_is_intersection() {
        let lhs = Rights::READ | Rights::WRITE | Rights::EXECUTE;
        let rhs = Rights::WRITE | Rights::DUPLICATE;
        assert_eq!(lhs & rhs, Rights::WRITE);
    }

    #[test]
    fn not_masks_to_known_bits_only() {
        // !READ должен содержать все остальные известные права и НИ ОДНОГО
        // неизвестного бита (маскирование ALL_BITS).
        let n = !Rights::READ;
        assert!(!n.contains(Rights::READ));
        assert!(n.contains(Rights::WRITE));
        assert!(n.contains(Rights::EXECUTE));
        assert!(n.contains(Rights::DUPLICATE));
        assert!(n.contains(Rights::TRANSFER));
        // Ни одного бита за пределами ALL_BITS.
        assert_eq!(n.bits() & !Rights::all().bits(), 0);
        // !ALL == NONE.
        assert_eq!(!Rights::all(), Rights::NONE);
        assert_eq!(!Rights::NONE, Rights::all());
    }

    #[test]
    fn into_u32_matches_bits() {
        let r = Rights::READ | Rights::TRANSFER;
        assert_eq!(u32::from(r), r.bits());
    }

    #[test]
    fn is_subset_of_holds_for_subset_and_fails_for_superset() {
        assert!(Rights::READ.is_subset_of(Rights::READ | Rights::WRITE));
        assert!(Rights::NONE.is_subset_of(Rights::READ));
        assert!(!(Rights::READ | Rights::WRITE).is_subset_of(Rights::READ));
        assert!(Rights::all().is_subset_of(Rights::all()));
    }
}
