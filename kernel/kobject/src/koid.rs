use core::{marker::PhantomData, num::NonZeroU64};

/// Маркер type-erased `Koid`.
pub enum Erased {}

/// Идентичность kernel-объекта: тип в старшем байте + адрес в младших 56 битах.
///
/// Стабильна, пока объект жив. После drop'а адрес может переиспользоваться,
/// поэтому Koid не глобально-уникален во времени.
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
            1 => "Channel",
            2 => "Event",
            3 => "Process",
            4 => "Thread",
            5 => "Memory",
            6 => "PhysicalResource",
            7 => "Mailbox",
            _ => "?",
        };
        write!(f, "Koid({name}#{:#x})", self.0.get() & ADDR_MASK)
    }
}
