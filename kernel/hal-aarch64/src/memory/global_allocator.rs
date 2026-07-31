//! Глобальный двухфазный аллокатор ядра.
//!
//! Сначала работает bump-аллокатор, затем переключается на heap.

use core::{
    alloc::{GlobalAlloc, Layout},
    cell::UnsafeCell,
    mem::MaybeUninit,
    ptr::NonNull,
    sync::atomic::{AtomicU8, Ordering},
};

use collections::{LockCell, MutexCell};
use memory::{bump_allocator::BumpAllocator, kernel_vm_allocator::HeapAllocator};

#[global_allocator]
pub(crate) static GLOBAL_ALLOCATOR: GlobalKernelAllocator = GlobalKernelAllocator::new();

/// Не инициализирован.
const PHASE_UNINIT: u8 = 0;
/// Bump-фаза (ранняя инициализация).
const PHASE_BUMP: u8 = 1;
/// Heap-фаза (основная работа).
const PHASE_HEAP: u8 = 2;
/// Заморожен (аллокации запрещены).
const PHASE_FROZEN: u8 = 3;

/// Двухфазный глобальный аллокатор ядра: bump-фаза без лока до включения
/// MMU, heap-фаза под локом с маскированием прерываний после.
pub struct GlobalKernelAllocator {
    /// Текущая фаза работы.
    phase: AtomicU8,
    /// Bump-аллокатор для ранней инициализации.
    bump: UnsafeCell<MaybeUninit<BumpAllocator>>,
    /// Heap-аллокатор для основной работы.
    heap: MutexCell<MaybeUninit<HeapAllocator>>,
}

// SAFETY: heap-фаза сериализована локом `heap`; bump-фаза однопоточна по
// построению - она заканчивается до включения MMU, то есть до появления
// прерываний и второго CPU. Переход между фазами публикуется атомарным
// `phase` с `Acquire`/`Release`-семантикой.
unsafe impl Sync for GlobalKernelAllocator {}

impl GlobalKernelAllocator {
    pub const fn new() -> Self {
        GlobalKernelAllocator {
            phase: AtomicU8::new(PHASE_UNINIT),
            bump: UnsafeCell::new(MaybeUninit::uninit()),
            heap: MutexCell::new(MaybeUninit::uninit()),
        }
    }

    /// Переключает на bump-фазу.
    pub fn set_bump(&self, bump_allocator: BumpAllocator) {
        let bump_ptr = self.bump.get();
        // SAFETY: caller вызывает `set_bump` ровно один раз в фазе UNINIT, до публикации
        // аллокатора через `Release`-store, эксклюзивный доступ к `MaybeUninit` гарантирован.
        unsafe {
            (*bump_ptr).write(bump_allocator);
        }
        self.phase.store(PHASE_BUMP, Ordering::Release);
    }

    /// Переключает на heap-фазу.
    pub fn set_heap(&self, heap_allocator: HeapAllocator) {
        self.heap.with_lock(|slot| {
            slot.write(heap_allocator);
        });
        self.phase.store(PHASE_HEAP, Ordering::Release);
    }

    /// Замораживает аллокатор. Аллокации после этого запрещены.
    pub fn set_freeze(&self) {
        self.phase.store(PHASE_FROZEN, Ordering::Release);
    }

    /// # Safety
    /// Вызывающий должен гарантировать, что bump-аллокатор инициализирован
    /// и не происходит конкурентного доступа.
    #[allow(clippy::mut_from_ref)]
    pub(crate) unsafe fn get_bump(&self) -> &mut BumpAllocator {
        // SAFETY: caller гарантирует инициализацию `bump` (фаза >= PHASE_BUMP) и отсутствие гонок;
        // `assume_init_mut` корректен.
        unsafe { (*self.bump.get()).assume_init_mut() }
    }

    /// # Safety
    /// Вызывающий должен гарантировать фазу `PHASE_HEAP`: в ней слот
    /// инициализирован.
    unsafe fn with_heap<R>(&self, f: impl FnOnce(&mut HeapAllocator) -> R) -> R {
        self.heap.with_lock(|slot| {
            // SAFETY: caller гарантирует фазу PHASE_HEAP, в которой `set_heap`
            // уже записал аллокатор в слот.
            f(unsafe { slot.assume_init_mut() })
        })
    }
}

// SAFETY: `alloc`/`dealloc` корректны: переходы между фазами защищены атомарным `phase` с
// `Acquire`/`Release`-семантикой, в каждой фазе соответствующий аллокатор инициализирован,
// а доступ к нему сериализован - к heap-фазе локом, к bump-фазе
// однопоточностью pre-MMU фазы.
unsafe impl GlobalAlloc for GlobalKernelAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        match self.phase.load(Ordering::Acquire) {
            // SAFETY: фаза PHASE_BUMP гарантирует, что bump инициализирован
            PHASE_BUMP => match unsafe { self.get_bump() }.allocate(layout) {
                Ok(ptr) => ptr.as_ptr(),
                Err(_) => core::ptr::null_mut(),
            },
            PHASE_FROZEN => {
                panic!("Allocation attempted while bump allocator is frozen")
            }
            // SAFETY: фаза PHASE_HEAP гарантирует, что heap инициализирован
            PHASE_HEAP => match unsafe { self.with_heap(|heap| heap.allocate(layout)) } {
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

        if self.phase.load(Ordering::Acquire) == PHASE_HEAP
            && let Some(non_null_ptr) = NonNull::new(ptr)
        {
            // SAFETY: фаза PHASE_HEAP гарантирует, что heap инициализирован
            unsafe { self.with_heap(|heap| heap.deallocate(non_null_ptr)) };
        }
    }
}
