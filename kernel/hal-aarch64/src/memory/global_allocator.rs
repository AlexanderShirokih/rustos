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

/// Двухфазный глобальный аллокатор ядра.
pub struct GlobalKernelAllocator {
    /// Текущая фаза работы.
    phase: AtomicU8,
    /// Bump-аллокатор для ранней инициализации.
    bump: UnsafeCell<MaybeUninit<BumpAllocator>>,
    /// Heap-аллокатор для основной работы.
    heap: UnsafeCell<MaybeUninit<HeapAllocator>>,
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
        let heap_ptr = self.heap.get();
        // SAFETY: caller вызывает `set_heap` ровно один раз перед переходом в PHASE_HEAP,
        // в этот момент конкурентного доступа к `heap` нет; затем фаза публикуется через Release.
        unsafe {
            (*heap_ptr).write(heap_allocator);
        }
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
    /// Вызывающий должен гарантировать, что heap-аллокатор инициализирован
    /// и не происходит конкурентного доступа.
    #[allow(clippy::mut_from_ref)]
    unsafe fn get_heap(&self) -> &mut HeapAllocator {
        // SAFETY: caller гарантирует инициализацию `heap` (фаза == PHASE_HEAP) и отсутствие гонок;
        // `assume_init_mut` корректен.
        unsafe { (*self.heap.get()).assume_init_mut() }
    }
}

// SAFETY: `alloc`/`dealloc` корректны: переходы между фазами защищены атомарным `phase` с
// `Acquire`/`Release`-семантикой, в каждой фазе соответствующий аллокатор инициализирован,
// сами аллокаторы (`BumpAllocator`, `HeapAllocator`) внутренне сериализуют доступ через
// исключительный `&mut` к статическому состоянию (ядро однопоточное на старте, после
// инициализации scheduler-а - единственный поток в kernel-space на CPU0 для allocate-ops).
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
            PHASE_HEAP => match unsafe { self.get_heap() }.allocate(layout) {
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
            unsafe { self.get_heap() }.deallocate(non_null_ptr);
        }
    }
}
