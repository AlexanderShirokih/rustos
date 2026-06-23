use core::{marker::PhantomData, num::NonZeroU64};

/// Маркер type-erased `Koid`.
pub enum Erased {}

/// Koid - диагностический идентификатор объекта ядра.
pub struct Koid<T: ?Sized = Erased>(NonZeroU64, PhantomData<fn() -> T>);

const TYPE_SHIFT: u32 = 56;
const ADDR_MASK: u64 = (1 << TYPE_SHIFT) - 1;

impl<T: ?Sized> Koid<T> {
    pub(crate) fn from_parts(tag: u8, addr: u64) -> Self {
        debug_assert!(tag != 0, "type tag must be non-zero");

        let raw = (u64::from(tag) << TYPE_SHIFT) | (addr & ADDR_MASK);
        Self(
            NonZeroU64::new(raw).expect("non-zero tag keeps top byte non-zero"),
            PhantomData,
        )
    }

    pub fn raw(self) -> u64 {
        self.0.get()
    }

    fn type_tag(self) -> u8 {
        (self.0.get() >> TYPE_SHIFT) as u8
    }

    pub fn erase(self) -> Koid {
        Koid(self.0, PhantomData)
    }
}

impl<T: ?Sized> Clone for Koid<T> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<T: ?Sized> Copy for Koid<T> {}

impl<T: ?Sized> PartialEq for Koid<T> {
    fn eq(&self, other: &Self) -> bool {
        self.0 == other.0
    }
}

impl<T: ?Sized> Eq for Koid<T> {}

impl<T: ?Sized> core::hash::Hash for Koid<T> {
    fn hash<H: core::hash::Hasher>(&self, state: &mut H) {
        self.0.hash(state);
    }
}

impl<T: ?Sized> core::fmt::Debug for Koid<T> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let name = match self.type_tag() {
            1 => "Signal",
            2 => "Process",
            3 => "Thread",
            4 => "Memory",
            5 => "Resource",
            6 => "Port",
            7 => "Reply",
            _ => "?",
        };
        write!(f, "Koid({name}#{:#x})", self.0.get() & ADDR_MASK)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_parts_preserves_tag_and_low_address_bits() {
        let k = Koid::<Erased>::from_parts(3, 0xDEAD_BEEF);
        assert_eq!(k.type_tag(), 3);
        assert_eq!(k.raw() & ADDR_MASK, 0xDEAD_BEEF);
    }

    #[test]
    fn distinct_addresses_yield_distinct_koids() {
        let a = Koid::<Erased>::from_parts(1, 0x1000);
        let b = Koid::<Erased>::from_parts(1, 0x2000);
        assert_ne!(a, b);
    }

    #[test]
    fn same_address_distinct_tags_yield_distinct_koids() {
        let proc = Koid::<Erased>::from_parts(2, 0x4000);
        let thread = Koid::<Erased>::from_parts(3, 0x4000);
        assert_ne!(proc, thread);
    }

    #[test]
    fn address_is_truncated_to_56_bits() {
        let k = Koid::<Erased>::from_parts(7, u64::MAX);
        assert_eq!(k.type_tag(), 7);
        assert_eq!(k.raw() & ADDR_MASK, ADDR_MASK);
    }
}
