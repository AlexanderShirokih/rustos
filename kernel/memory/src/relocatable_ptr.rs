#![allow(unsafe_code)]

use crate::virtual_address::VirtualAddress;

/// Указатель, который можно релоцировать между адресными пространствами.
///
/// Работает с fat pointers - `dyn Trait` и срезами. Bound `T: ?Sized` пропускает и
/// `Sized`-типы, но для них `new` читает `[usize; 2]` из однословного `*const T` -
/// подставлять `Sized T` нельзя.
pub struct RelocatablePtr<T: ?Sized> {
    raw: [usize; 2],
    _marker: core::marker::PhantomData<*const T>,
}

impl<T: ?Sized> RelocatablePtr<T> {
    pub fn new(r: &'static T) -> Self {
        // Копирование fat pointer как [data, vtable]
        // SAFETY: для действительно безразмерного `T` (dyn Trait, срез) `*const T` -
        // пара usize (data, vtable/len), того же размера, что и `[usize; 2]`, поэтому
        // `transmute_copy` не читает за пределы источника и не нарушает алиасинга.
        // Вызывающий `new` отвечает за то, что `T` не `Sized` - bound `T: ?Sized`
        // это не проверяет.
        let raw: [usize; 2] = unsafe { core::mem::transmute_copy(&core::ptr::from_ref::<T>(r)) };
        Self {
            raw,
            _marker: core::marker::PhantomData,
        }
    }

    /// Релоцирует указатель на заданное смещение.
    ///
    /// # Safety
    ///
    /// Память по новому адресу должна быть замаплена и содержать валидный объект типа T.
    pub unsafe fn relocated(self, new_base: VirtualAddress) -> &'static T {
        let offset = new_base.as_usize();
        // Fat pointer = [data_ptr, vtable_ptr]
        let relocated_raw = [self.raw[0] + offset, self.raw[1] + offset];
        // SAFETY: `relocated_raw` имеет размер и layout `*const T`, поэтому
        // `transmute_copy` корректно собирает fat pointer из пары usize.
        let relocated_ptr: *const T = unsafe { core::mem::transmute_copy(&relocated_raw) };
        // SAFETY: caller гарантирует, что новый базовый адрес замаплен и хранит
        // валидный объект `T` (контракт `unsafe fn`).
        unsafe { &*relocated_ptr }
    }

    pub fn get(&self) -> &'static T {
        // SAFETY: `self.raw` создан в `new` из `*const T` и не изменялся; layout
        // пары usize совпадает с `*const T`.
        let ptr: *const T = unsafe { core::mem::transmute_copy(&self.raw) };
        // SAFETY: указатель получен из живой `&'static T`-ссылки в `new` и
        // остаётся валидным на всё время жизни `'static`.
        unsafe { &*ptr }
    }
}
