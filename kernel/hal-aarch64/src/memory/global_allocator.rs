//! Глобальный двухфазный аллокатор ядра.
//!
//! Сначала работает bump-аллокатор, затем переключается на heap. В heap-фазе кеш
//! текущего CPU позволяет повторно использовать небольшие блоки без общей блокировки
//! кучи. При отсутствии подходящего блока в кеше или места для его возврата
//! аллокатор обращается к общей куче.

use core::{
    alloc::{GlobalAlloc, Layout},
    cell::UnsafeCell,
    mem::MaybeUninit,
    ptr::NonNull,
    sync::atomic::{AtomicU8, AtomicUsize, Ordering},
};

use kbase::{LockCell, MutexCell};
use memory::{bump_allocator::BumpAllocator, kernel_vm_allocator::HeapAllocator};
use scheduler::MAX_CPUS;

use crate::memory::regs::{common::EL1, dcache::DataCache};

#[global_allocator]
pub(crate) static GLOBAL_ALLOCATOR: GlobalKernelAllocator = GlobalKernelAllocator::new();

const PHASE_UNINIT: u8 = 0;
const PHASE_BUMP: u8 = 1;
const PHASE_HEAP: u8 = 2;
const PHASE_FROZEN: u8 = 3;

const CACHE_MIN_SHIFT: u32 = 4;
const CACHE_CLASSES: usize = 10;
const CACHE_ALIGN: usize = 16;
const CACHE_CLASS_BYTES: usize = 64 * 1024;
const CACHE_CLASS_MIN_BLOCKS: usize = 32;

pub(crate) struct GlobalKernelAllocator {
    phase: AtomicU8,
    bump: UnsafeCell<MaybeUninit<BumpAllocator>>,
    heap: MutexCell<MaybeUninit<HeapAllocator>>,
    arena_start: AtomicUsize,
    arena_end: AtomicUsize,
    caches: [CpuCache; MAX_CPUS],
}

#[repr(align(64))]
struct CpuCache {
    lists: MutexCell<CacheLists>,
}

struct CacheLists {
    heads: [Option<NonNull<CachedBlock>>; CACHE_CLASSES],
    counts: [usize; CACHE_CLASSES],
}

struct CachedBlock {
    next: Option<NonNull<CachedBlock>>,
}

// SAFETY: heap-фаза сериализована локом `heap`, кеш CPU - локом своей ячейки;
// bump-фаза однопоточна по построению - она заканчивается до включения MMU, то
// есть до появления прерываний и второго CPU. Переход между фазами публикуется
// атомарным `phase` с `Acquire`/`Release`-семантикой.
unsafe impl Sync for GlobalKernelAllocator {}

// SAFETY: блоки списков принадлежат кешу и доступны только под локом его ячейки.
unsafe impl Send for CacheLists {}

impl GlobalKernelAllocator {
    pub(crate) const fn new() -> Self {
        GlobalKernelAllocator {
            phase: AtomicU8::new(PHASE_UNINIT),
            bump: UnsafeCell::new(MaybeUninit::uninit()),
            heap: MutexCell::new(MaybeUninit::uninit()),
            arena_start: AtomicUsize::new(0),
            arena_end: AtomicUsize::new(0),
            caches: [const {
                CpuCache {
                    lists: MutexCell::new(CacheLists {
                        heads: [None; CACHE_CLASSES],
                        counts: [0; CACHE_CLASSES],
                    }),
                }
            }; MAX_CPUS],
        }
    }

    pub(crate) fn set_bump(&self, bump_allocator: BumpAllocator) {
        let bump_ptr = self.bump.get();
        // SAFETY: caller вызывает `set_bump` ровно один раз в фазе UNINIT, до публикации
        // аллокатора через `Release`-store, эксклюзивный доступ к `MaybeUninit` гарантирован.
        unsafe {
            (*bump_ptr).write(bump_allocator);
        }
        self.phase.store(PHASE_BUMP, Ordering::Release);
    }

    pub(crate) fn set_heap(&self, heap_allocator: HeapAllocator) {
        let arena = heap_allocator.arena();
        let start = arena.base.as_usize();
        self.arena_start.store(start, Ordering::Relaxed);
        self.arena_end
            .store(start.saturating_add(arena.max_size), Ordering::Relaxed);
        self.heap.with_lock(|slot| {
            slot.write(heap_allocator);
        });
        self.phase.store(PHASE_HEAP, Ordering::Release);
    }

    pub(crate) fn set_freeze(&self) {
        self.phase.store(PHASE_FROZEN, Ordering::Release);
    }

    // SAFETY: caller держит фазу >= PHASE_BUMP, в которой bump-аллокатор
    // инициализирован, и не допускает конкурентного доступа к нему на время
    // вызова `f`; `f` не выполняет аллокацию, потому что повторный вход даёт
    // вторую `&mut` на слот.
    pub(crate) unsafe fn with_bump<R>(&self, f: impl FnOnce(&mut BumpAllocator) -> R) -> R {
        // SAFETY: caller гарантирует отсутствие других ссылок на слот на время
        // вызова; `UnsafeCell::get` отдаёт валидный указатель на него.
        let slot = unsafe { &mut *self.bump.get() };
        // SAFETY: caller гарантирует фазу >= PHASE_BUMP, в которой `set_bump`
        // уже записал аллокатор в слот.
        f(unsafe { slot.assume_init_mut() })
    }

    // SAFETY: caller держит фазу PHASE_HEAP, в которой слот heap инициализирован.
    unsafe fn with_heap<R>(&self, f: impl FnOnce(&mut HeapAllocator) -> R) -> R {
        self.heap.with_lock(|slot| {
            // SAFETY: caller гарантирует фазу PHASE_HEAP, в которой `set_heap`
            // уже записал аллокатор в слот.
            f(unsafe { slot.assume_init_mut() })
        })
    }

    fn current_cache(&self) -> Option<&CpuCache> {
        self.caches.get(crate::cpu::current()?.id().as_index())
    }

    fn in_arena(&self, ptr: NonNull<u8>) -> bool {
        let addr = ptr.as_ptr() as usize;
        addr >= self.arena_start.load(Ordering::Relaxed)
            && addr < self.arena_end.load(Ordering::Relaxed)
    }

    // SAFETY: caller держит фазу PHASE_HEAP.
    unsafe fn heap_alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: caller гарантирует фазу PHASE_HEAP, в которой heap инициализирован.
        match unsafe { self.with_heap(|heap| heap.allocate(layout)) } {
            Ok(ptr) => ptr.as_ptr(),
            Err(_) => core::ptr::null_mut(),
        }
    }
}

impl CpuCache {
    fn pop(&self, class: usize) -> Option<NonNull<u8>> {
        self.lists.with_lock(|lists| {
            let head = lists.heads.get_mut(class)?;
            let block = (*head)?;
            // SAFETY: блок списка записан `try_push` и до выдачи принадлежит кешу.
            *head = unsafe { block.as_ref().next };
            if let Some(count) = lists.counts.get_mut(class) {
                *count = count.saturating_sub(1);
            }
            Some(block.cast())
        })
    }

    fn try_push(&self, class: usize, ptr: NonNull<u8>) -> bool {
        self.lists.with_lock(|lists| {
            let (Some(head), Some(count)) =
                (lists.heads.get_mut(class), lists.counts.get_mut(class))
            else {
                return false;
            };
            if *count >= class_capacity(class) {
                return false;
            }
            let block = ptr.cast::<CachedBlock>();
            // SAFETY: блок класса имеет не меньше 16 байт и выравнивание `CACHE_ALIGN`,
            // а владелец отдал его аллокатору.
            unsafe { block.as_ptr().write(CachedBlock { next: *head }) };
            *head = Some(block);
            *count = count.saturating_add(1);
            true
        })
    }
}

fn cache_class(layout: Layout) -> Option<usize> {
    if layout.align() > CACHE_ALIGN {
        return None;
    }
    let size = layout
        .size()
        .max(1 << CACHE_MIN_SHIFT)
        .checked_next_power_of_two()?;
    let class = usize::try_from(size.trailing_zeros().checked_sub(CACHE_MIN_SHIFT)?).ok()?;
    (class < CACHE_CLASSES).then_some(class)
}

fn class_layout(class: usize) -> Option<Layout> {
    let size = (1usize << CACHE_MIN_SHIFT).checked_shl(u32::try_from(class).ok()?)?;
    Layout::from_size_align(size, CACHE_ALIGN).ok()
}

fn class_capacity(class: usize) -> usize {
    u32::try_from(class)
        .ok()
        .and_then(|class| (CACHE_CLASS_BYTES >> CACHE_MIN_SHIFT).checked_shr(class))
        .unwrap_or(0)
        .max(CACHE_CLASS_MIN_BLOCKS)
}

// SAFETY: `alloc`/`dealloc` корректны: переходы между фазами защищены атомарным `phase` с
// `Acquire`/`Release`-семантикой, в каждой фазе соответствующий аллокатор инициализирован,
// а доступ к нему сериализован - к heap-фазе локом, к bump-фазе
// однопоточностью pre-MMU фазы. Кеш CPU хранит только блоки арены, выделенные
// размером своего класса.
unsafe impl GlobalAlloc for GlobalKernelAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        match self.phase.load(Ordering::Acquire) {
            // SAFETY: фаза PHASE_BUMP гарантирует, что bump инициализирован, а
            // до включения MMU доступ к нему однопоточен.
            PHASE_BUMP => match unsafe { self.with_bump(|bump| bump.allocate(layout)) } {
                Ok(ptr) => {
                    let start = ptr.as_ptr() as usize;
                    // SAFETY: bump-фаза идёт до включения MMU, и адрес физический. Хвост
                    // прошлого выделения в первой строке сброшен до его первой записи.
                    unsafe {
                        DataCache::<EL1>::invalidate_range_to_coherency(
                            start,
                            start + layout.size(),
                        );
                    }
                    ptr.as_ptr()
                }
                Err(_) => core::ptr::null_mut(),
            },
            PHASE_FROZEN => {
                panic!("Allocation attempted while bump allocator is frozen")
            }
            PHASE_HEAP => {
                let Some(class) = cache_class(layout) else {
                    // SAFETY: фаза PHASE_HEAP.
                    return unsafe { self.heap_alloc(layout) };
                };
                if let Some(block) = self.current_cache().and_then(|cache| cache.pop(class)) {
                    return block.as_ptr();
                }
                match class_layout(class) {
                    // SAFETY: фаза PHASE_HEAP.
                    Some(class_layout) => unsafe { self.heap_alloc(class_layout) },
                    None => core::ptr::null_mut(),
                }
            }
            _ => core::ptr::null_mut(),
        }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        if ptr.is_null() {
            return;
        }

        if self.phase.load(Ordering::Acquire) == PHASE_HEAP
            && let Some(non_null_ptr) = NonNull::new(ptr)
        {
            if let Some(class) = cache_class(layout)
                && self.in_arena(non_null_ptr)
                && let Some(cache) = self.current_cache()
                && cache.try_push(class, non_null_ptr)
            {
                return;
            }
            // SAFETY: фаза PHASE_HEAP гарантирует, что heap инициализирован
            unsafe { self.with_heap(|heap| heap.deallocate(non_null_ptr)) };
        }
    }
}
