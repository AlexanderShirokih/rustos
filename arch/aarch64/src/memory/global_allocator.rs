use core::alloc::{GlobalAlloc, Layout};
use core::cell::UnsafeCell;
use core::mem::MaybeUninit;
use core::ptr::NonNull;
use core::sync::atomic::{AtomicU8, Ordering};
use memory::bump_allocator::BumpAllocator;
use memory::heap_allocator::HeapAllocator;

/// Глобальный двухфазный аллокатор ядра
#[global_allocator]
pub(crate) static GLOBAL_ALLOCATOR: GlobalKernelAllocator = GlobalKernelAllocator::new();

/// Фазы работы аллокатора
const PHASE_UNINIT: u8 = 0;
const PHASE_BUMP: u8 = 1;
const PHASE_HEAP: u8 = 2;
const PHASE_FROZEN: u8 = 3;

pub type KernelHeapAllocator = HeapAllocator;

/// Двухфазный глобальный аллокатор ядра
pub struct GlobalKernelAllocator {
    /// Текущая фаза
    phase: AtomicU8,
    /// Bump-аллокатор для ранней инициализации
    bump: UnsafeCell<MaybeUninit<BumpAllocator>>,
    /// Полноценный heap-аллокатор
    heap: UnsafeCell<MaybeUninit<KernelHeapAllocator>>,
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
    pub fn set_bump(&self, bump_allocator: BumpAllocator) {
        let bump_ptr = self.bump.get();
        unsafe {
            (*bump_ptr).write(bump_allocator);
        }
        self.phase.store(PHASE_BUMP, Ordering::Release);
    }

    /// Переключает аллокатор на heap фазу работы
    pub fn set_heap(&self, heap_allocator: KernelHeapAllocator) {
        let heap_ptr = self.heap.get();
        unsafe {
            (*heap_ptr).write(heap_allocator);
        }
        self.phase.store(PHASE_HEAP, Ordering::Release);
    }

    /// Замораживает аллокатор. После этого аллокации невозможны
    pub fn set_freeze(&self) {
        self.phase.store(PHASE_FROZEN, Ordering::Release);
    }

    pub(crate) fn get_bump(&self) -> &mut BumpAllocator {
        unsafe { (*self.bump.get()).assume_init_mut() }
    }

    fn get_heap(&self) -> &mut KernelHeapAllocator {
        unsafe { (*self.heap.get()).assume_init_mut() }
    }
}

unsafe impl GlobalAlloc for GlobalKernelAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        match self.phase.load(Ordering::Acquire) {
            PHASE_BUMP => match self.get_bump().allocate(layout) {
                Ok(ptr) => ptr.as_ptr(),
                Err(_) => core::ptr::null_mut(),
            },
            PHASE_FROZEN => {
                panic!("Allocation attempted while bump allocator is frozen")
            }
            PHASE_HEAP => match self.get_heap().allocate(layout) {
                Ok(ptr) => ptr.as_ptr(),
                Err(_) => core::ptr::null_mut(),
            },
            _ => core::ptr::null_mut(),
        }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, _layout: Layout) {
        if ptr.is_null() {
            return;
        }

        match self.phase.load(Ordering::Acquire) {
            PHASE_HEAP => {
                if let Some(non_null_ptr) = NonNull::new(ptr) {
                    self.get_heap().deallocate(non_null_ptr);
                }
            }
            _ => {
                // no-op
            }
        }
    }
}
