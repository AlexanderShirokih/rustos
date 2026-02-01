use crate::virtual_address::VirtualAddress;

/// Указатель, который можно релоцировать между адресными пространствами.
/// Корректно работает с fat pointers (dyn Trait).
pub struct RelocatablePtr<T: ?Sized> {
    // Храним fat pointer как пару [data, vtable]
    raw: [usize; 2],
    _marker: core::marker::PhantomData<*const T>,
}

impl<T: ?Sized> RelocatablePtr<T> {
    pub fn new(r: &'static T) -> Self {
        // Копируем fat pointer как [data, vtable]
        let raw: [usize; 2] = unsafe { core::mem::transmute_copy(&(r as *const T)) };
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
        unsafe {
            let relocated_ptr: *const T = core::mem::transmute_copy(&relocated_raw);
            &*relocated_ptr
        }
    }

    pub fn get(&self) -> &'static T {
        unsafe {
            let ptr: *const T = core::mem::transmute_copy(&self.raw);
            &*ptr
        }
    }
}
