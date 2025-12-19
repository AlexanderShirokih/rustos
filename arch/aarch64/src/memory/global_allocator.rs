use crate::memory::memory_mapper::Aarch64MemoryMapper;
use crate::memory::ram_memory::Aarch64VirtualRamMemory;
use collections::NoLockCell;
use core::alloc::{GlobalAlloc, Layout};
use core::cell::UnsafeCell;
use core::mem::{MaybeUninit, size_of};
use core::ptr::NonNull;
use core::sync::atomic::{AtomicU8, Ordering};
use memory::FrameBitmap;
use memory::bump_allocator::BumpAllocator;
use memory::frame_allocator::PhysicalFrameAllocator;
use memory::heap_allocator::HeapAllocator;

/// Фазы работы аллокатора
const PHASE_UNINIT: u8 = 0;
const PHASE_BUMP: u8 = 1;
const PHASE_HEAP: u8 = 2;

pub type EarlyKernelHeapAllocator = HeapAllocator<
    Aarch64VirtualRamMemory,
    Aarch64MemoryMapper<'static, PhysicalFrameAllocator<NoLockCell<FrameBitmap>>>,
>;

/// Двухфазный глобальный аллокатор ядра
pub struct GlobalKernelAllocator {
    /// Текущая фаза: 0 = не инициализирован, 1 = bump, 2 = heap
    phase: AtomicU8,
    /// Bump-аллокатор для ранней инициализации
    bump: UnsafeCell<MaybeUninit<BumpAllocator>>,
    /// Полноценный heap-аллокатор
    heap: UnsafeCell<MaybeUninit<EarlyKernelHeapAllocator>>,
}

// SAFETY: GlobalKernelAllocator использует атомарные операции для синхронизации
// и гарантирует, что только одна фаза активна в любой момент времени
unsafe impl Sync for GlobalKernelAllocator {}

impl GlobalKernelAllocator {
    pub const fn new() -> Self {
        GlobalKernelAllocator {
            phase: AtomicU8::new(PHASE_UNINIT),
            bump: UnsafeCell::new(MaybeUninit::uninit()),
            heap: UnsafeCell::new(MaybeUninit::uninit()),
        }
    }

    /// Инициализирует bump фазу аллокатор
    pub fn init_bump_phase(&self, bump_allocator: BumpAllocator) {
        let bump_ptr = self.bump.get();
        unsafe {
            (*bump_ptr).write(bump_allocator);
        }
        self.phase.store(PHASE_BUMP, Ordering::Release);
    }

    /// Переключает аллокатор на heap фазу работы
    pub fn switch_to_heap(&self, heap_allocator: EarlyKernelHeapAllocator) {
        let heap_ptr = self.heap.get();
        unsafe {
            (*heap_ptr).write(heap_allocator);
        }
        self.phase.store(PHASE_HEAP, Ordering::Release);
    }

    fn bump_allocator(&self) -> &mut BumpAllocator {
        unsafe { (*self.bump.get()).assume_init_mut() }
    }

    fn heap_allocator(&self) -> &mut EarlyKernelHeapAllocator {
        unsafe { (*self.heap.get()).assume_init_mut() }
    }
}

unsafe impl GlobalAlloc for GlobalKernelAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        match self.phase.load(Ordering::Acquire) {
            PHASE_BUMP => match self.bump_allocator().allocate(layout) {
                Ok(ptr) => ptr.as_ptr(),
                Err(_) => core::ptr::null_mut(),
            },
            PHASE_HEAP => match self.heap_allocator().allocate(layout) {
                Ok(ptr) => ptr.as_ptr(),
                Err(_) => core::ptr::null_mut(),
            },
            _ => {
                // Аллокатор не инициализирован
                core::ptr::null_mut()
            }
        }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, _layout: Layout) {
        if ptr.is_null() {
            return;
        }

        match self.phase.load(Ordering::Acquire) {
            PHASE_BUMP => {
                // Bump-фаза: освобождение памяти - no-op
            }
            PHASE_HEAP => {
                let bump_start = self.bump.get() as usize;
                let bump_end = bump_start + size_of::<BumpAllocator>();
                let ptr_addr = ptr as usize;

                if ptr_addr >= bump_start && ptr_addr < bump_end {
                    // Память из bump-аллокатора - не освобождаем
                    return;
                }

                if let Some(non_null_ptr) = NonNull::new(ptr) {
                    self.heap_allocator().deallocate(non_null_ptr);
                }
            }
            _ => {
                // Аллокатор не инициализирован - игнорируем
            }
        }
    }
}
