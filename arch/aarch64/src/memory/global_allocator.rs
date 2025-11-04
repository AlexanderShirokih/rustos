//! Глобальный аллокатор без использования статических полей
//!
//! Этот модуль предоставляет глобальный аллокатор, который можно использовать
//! с атрибутом #[global_allocator]. Он хранит только указатель на HeapAllocator,
//! который владеется MemoryManager.

use crate::memory::allocator::{HeapAllocator, MemoryMapper};
use core::alloc::{GlobalAlloc, Layout};
use core::ptr::NonNull;
use core::sync::atomic::{AtomicPtr, Ordering};
use memory::memory_backend::MemoryBackend;
use memory::physical_manager::PhysicalMemoryManager;

/// Глобальный аллокатор, который хранит указатель на HeapAllocator
pub struct GlobalKernelAllocator {
    heap_allocator: AtomicPtr<u8>,
}

impl GlobalKernelAllocator {
    pub const fn new() -> Self {
        GlobalKernelAllocator {
            heap_allocator: AtomicPtr::new(core::ptr::null_mut()),
        }
    }

    pub unsafe fn set_heap_allocator<B: MemoryBackend, M: MemoryMapper>(
        &self,
        allocator: *mut HeapAllocator<B, M>,
    ) {
        self.heap_allocator
            .store(allocator as *mut u8, Ordering::Release);
    }

    fn get_heap_allocator<B: MemoryBackend, M: MemoryMapper>(
        &self,
    ) -> Option<&mut HeapAllocator<B, M>> {
        let ptr = self.heap_allocator.load(Ordering::Acquire);
        if ptr.is_null() {
            None
        } else {
            unsafe { Some(&mut *(ptr as *mut HeapAllocator<B, M>)) }
        }
    }
}

unsafe impl<'a> GlobalAlloc for GlobalKernelAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // Получаем типизированный указатель на HeapAllocator
        // Здесь мы используем конкретные типы, которые используются в системе
        use crate::memory::memory_mapper::Aarch64MemoryMapper;
        use crate::memory::ram_memory::Aarch64RamMemory;

        if let Some(allocator) =
            self.get_heap_allocator::<Aarch64RamMemory, Aarch64MemoryMapper<PhysicalMemoryManager<'a, Aarch64RamMemory>, Aarch64RamMemory>>()
        {
            match allocator.allocate(layout) {
                Ok(ptr) => ptr.as_ptr(),
                Err(_) => core::ptr::null_mut(),
            }
        } else {
            // Аллокатор не инициализирован
            core::ptr::null_mut()
        }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, _layout: Layout) {
        if ptr.is_null() {
            return;
        }

        use crate::memory::memory_mapper::Aarch64MemoryMapper;
        use crate::memory::ram_memory::Aarch64RamMemory;

        if let Some(allocator) =
            self.get_heap_allocator::<Aarch64RamMemory, Aarch64MemoryMapper<PhysicalMemoryManager<Aarch64RamMemory>, Aarch64RamMemory>>()
        {
            if let Some(non_null_ptr) = NonNull::new(ptr) {
                allocator.deallocate(non_null_ptr);
            }
        }
    }
}
