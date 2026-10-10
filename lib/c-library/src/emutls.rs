//! Thread-local переменные C и C++ через emutls компилятора: `__emutls_get_address`.
//!
//! Поток получает собственный экземпляр переменной при первом обращении к ней. Экземпляры
//! хранятся в записи потока и освобождаются вместе с ней после терминации потока.

#![allow(unsafe_code)]

use alloc::{
    alloc::{alloc, dealloc},
    vec::Vec,
};
use core::{
    alloc::Layout,
    cell::UnsafeCell,
    ptr::{self, NonNull},
    sync::atomic::{
        AtomicUsize,
        Ordering::{AcqRel, Acquire, Relaxed},
    },
};

use crate::{process, thread};

static NEXT_INDEX: AtomicUsize = AtomicUsize::new(1);

#[repr(C)]
struct Control {
    size: usize,
    align: usize,
    index: AtomicUsize,
    template: *const u8,
}

pub(crate) struct Instances {
    slots: UnsafeCell<Vec<Option<Instance>>>,
}

struct Instance {
    data: NonNull<u8>,
    layout: Layout,
}

impl Control {
    fn index(&self) -> usize {
        let index = self.index.load(Acquire);
        // Нулевой индекс означает, что переменной индекс ещё не выдан.
        if index != 0 {
            return index;
        }

        // Индекс проигравшего гонку потока пропадает, и его место в массивах остаётся пустым.
        let fresh = NEXT_INDEX.fetch_add(1, Relaxed);
        match self.index.compare_exchange(0, fresh, AcqRel, Acquire) {
            Ok(_) => fresh,
            Err(assigned) => assigned,
        }
    }

    #[allow(clippy::multiple_unsafe_ops_per_block)]
    fn instantiate(&self) -> Instance {
        let Ok(layout) = Layout::from_size_align(self.size.max(1), self.align.max(1)) else {
            process::abort_with("thread-local variable has an invalid layout");
        };
        // SAFETY: размер `layout` ненулевой.
        let Some(data) = NonNull::new(unsafe { alloc(layout) }) else {
            process::abort_with("thread-local variable instance is out of memory");
        };

        // SAFETY: блок `data` занимает не меньше `size` байт. Непустой шаблон компилятор кладёт
        // в данные программы размером `size`, и с новым блоком он не пересекается.
        unsafe {
            if self.template.is_null() {
                ptr::write_bytes(data.as_ptr(), 0, self.size);
            } else {
                ptr::copy_nonoverlapping(self.template, data.as_ptr(), self.size);
            }
        }
        Instance { data, layout }
    }
}

impl Instances {
    pub(crate) const fn new() -> Self {
        Self {
            slots: UnsafeCell::new(Vec::new()),
        }
    }

    unsafe fn address(&self, control: &Control) -> *mut u8 {
        let position = control.index() - 1;
        // SAFETY: вызывающий - поток, которому принадлежит массив; массив меняет только его
        // поток, и ссылка не покидает вызов.
        let slots = unsafe { &mut *self.slots.get() };
        if slots.len() <= position {
            if slots.try_reserve(position + 1 - slots.len()).is_err() {
                process::abort_with("thread-local variable instances are out of memory");
            }
            slots.resize_with(position + 1, || None);
        }

        match slots.get_mut(position) {
            Some(Some(instance)) => instance.data.as_ptr(),
            Some(slot) => slot.insert(control.instantiate()).data.as_ptr(),
            None => process::abort_with("thread-local variable slot is missing"),
        }
    }
}

impl Drop for Instances {
    fn drop(&mut self) {
        for instance in self.slots.get_mut().drain(..).flatten() {
            // SAFETY: блок выдан `alloc` с этим `layout` в `Control::instantiate`.
            unsafe { dealloc(instance.data.as_ptr(), instance.layout) };
        }
    }
}

#[unsafe(no_mangle)]
extern "C" fn __emutls_get_address(control: &Control) -> *mut u8 {
    // SAFETY: запись `current` принадлежит вызывающему потоку.
    unsafe { thread::current().instances().address(control) }
}
