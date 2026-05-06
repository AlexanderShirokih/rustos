use core::{marker::PhantomData, num::NonZeroU64};

use super::{kernel_object::KernelObject, object_type::ObjectType};

/// Маркер type-erased `Koid`.
pub enum Erased {}

/// Идентичность kernel-объекта: тип в старшем байте + адрес в младших 56 битах.
///
/// Стабильна, пока объект жив. После drop'а адрес может переиспользоваться,
/// поэтому `Koid` не глобально-уникален во времени - это сознательный
/// компромисс ради нулевого оверхеда (нет поля, нет глобального счётчика).
pub struct Koid<T: ?Sized = Erased>(NonZeroU64, PhantomData<fn() -> T>);

const TYPE_SHIFT: u32 = 56;
const ADDR_MASK: u64 = (1 << TYPE_SHIFT) - 1;

impl<T: ?Sized> Koid<T> {
    pub fn raw(self) -> u64 {
        self.0.get()
    }

    pub fn object_type(self) -> ObjectType {
        let tag = (self.0.get() >> TYPE_SHIFT) as u8;
        ObjectType::from_u8(tag).expect("koid carries a valid ObjectType tag")
    }

    pub fn erase(self) -> Koid {
        Koid(self.0, PhantomData)
    }
}

impl<T: KernelObject + ?Sized> Koid<T> {
    pub fn of(obj: &T) -> Self {
        let addr = obj as *const T as *const () as u64;
        let raw = ((obj.object_type() as u64) << TYPE_SHIFT) | (addr & ADDR_MASK);
        Self(
            NonZeroU64::new(raw).expect("ObjectType >= 1 keeps top byte non-zero"),
            PhantomData,
        )
    }
}

// Manual impls - #[derive] требует T: Trait, а нам нужны эти trait'ы для
// любого ?Sized T (включая dyn KernelObject), независимо от bound'ов на T.
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
        write!(
            f,
            "Koid({:?}#{:#x})",
            self.object_type(),
            self.0.get() & ADDR_MASK
        )
    }
}
