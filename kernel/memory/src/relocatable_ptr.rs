#![allow(unsafe_code)]

use crate::virtual_address::VirtualAddress;

/// Указатель, который можно релоцировать между адресными пространствами.
///
/// Корректно работает с fat pointers (dyn Trait).
pub struct RelocatablePtr<T: ?Sized> {
    /// Fat pointer как пара [data_ptr, vtable_ptr].
    raw: [usize; 2],
    /// Маркер для типа T.
    _marker: core::marker::PhantomData<*const T>,
}

impl<T: ?Sized> RelocatablePtr<T> {
    pub fn new(r: &'static T) -> Self {
        // Копирование fat pointer как [data, vtable]
        // SAFETY: `*const T` для `T: ?Sized` представляется парой usize (data, vtable);
        // `transmute_copy` читает ровно `size_of::<*const T>()` байт из локальной переменной
        // того же размера, не нарушая алиасинга.
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
        // SAFETY: caller гарантирует, что новый базовый адрес замаплен и хранит валидный объект `T`
        // (контракт `unsafe fn`); `relocated_raw` имеет размер и layout `*const T`,
        // `transmute_copy` корректно собирает fat pointer из пары usize.
        unsafe {
            let relocated_ptr: *const T = core::mem::transmute_copy(&relocated_raw);
            &*relocated_ptr
        }
    }

    pub fn get(&self) -> &'static T {
        // SAFETY: `self.raw` был создан в `new` из живой `&'static T`-ссылки и не изменялся;
        // указатель остаётся валидным на всё время жизни `'static`, layout пары usize совпадает с `*const T`.
        unsafe {
            let ptr: *const T = core::mem::transmute_copy(&self.raw);
            &*ptr
        }
    }
}
