//! Глобальный heap процесса: talc поверх memory_allocate/memory_free.
//!
//! Аллокации свыше PASSTHROUGH_THRESHOLD идут напрямую к ядру и возвращаются на
//! free; мелкие обслуживает talc, растущий чанками от ядра.

use core::{
    alloc::{GlobalAlloc, Layout},
    ptr::{self, NonNull},
    sync::atomic::{AtomicU32, Ordering},
};

use syscall::{Handle, MEM_FLAGS_READ_WRITE};
use talc::{OomHandler, Span, Talc};

use crate::{Mutex, memory_allocate, memory_free, process_resource_self};

/// Кэш метеринг-handle: `process_resource_self` ставит свежий handle на каждый вызов,
/// поэтому кэшируем первый (на старт-гонке лишний handle безвреден).
static METERING_HANDLE: AtomicU32 = AtomicU32::new(0);

fn metering_handle() -> Option<Handle> {
    let cached = METERING_HANDLE.load(Ordering::Acquire);
    if cached != 0 {
        return Handle::new(cached);
    }
    let raw = process_resource_self().ok()?.raw();
    match METERING_HANDLE.compare_exchange(0, raw, Ordering::AcqRel, Ordering::Acquire) {
        Ok(_) => Handle::new(raw),
        Err(existing) => Handle::new(existing),
    }
}

/// Гранулярность маппинга ядра.
const PAGE_SIZE: usize = 4096;

/// Все аллокации свыше этого размера будут обслуживаться напрямую через
/// memory_allocate/memory_free, минуя talc.
const PASSTHROUGH_THRESHOLD: usize = 64 * 1024;

/// Размер первого чанка арены talc.
const FIRST_CHUNK: usize = 64 * 1024;

/// Потолок чанка: claim растёт геометрически до этого значения.
const MAX_CHUNK: usize = 1024 * 1024;

const fn align_up(value: usize, align: usize) -> usize {
    value.saturating_add(align - 1) & !(align - 1)
}

/// OOM-обработчик talc: запрашивает у ядра новый чанк и отдаёт его через claim.
/// Чанк растёт геометрически от FIRST_CHUNK до MAX_CHUNK.
struct SyscallOom {
    /// Размер следующего claim-чанка.
    next_chunk: usize,
}

impl OomHandler for SyscallOom {
    fn handle_oom(talc: &mut Talc<Self>, layout: Layout) -> Result<(), ()> {
        // Чанк обязан вместить аллокацию с метаданными talc; для sub-threshold
        // layout это всегда покрывает шаг, max - страховка.
        let required = layout
            .size()
            .saturating_add(layout.align())
            .saturating_add(PAGE_SIZE);
        let bytes = align_up(required.max(talc.oom_handler.next_chunk), PAGE_SIZE);

        let resource = metering_handle().ok_or(())?;
        let va = memory_allocate(resource, bytes as u64, MEM_FLAGS_READ_WRITE);
        let base = match usize::try_from(va) {
            Ok(addr) if addr != 0 => addr as *mut u8,
            _ => return Err(()),
        };

        // SAFETY: [base, base+bytes) только что замаплен ядром как RW, не
        // пересекается с другими аренами и не содержит null.
        unsafe {
            talc.claim(Span::from_base_size(base, bytes))?;
        }

        talc.oom_handler.next_chunk = talc.oom_handler.next_chunk.saturating_mul(2).min(MAX_CHUNK);
        Ok(())
    }
}

/// Глобальный аллокатор процесса
pub(crate) struct ProcessHeap {
    talc: Mutex<Talc<SyscallOom>>,
}

impl ProcessHeap {
    pub(crate) const fn new() -> Self {
        Self {
            talc: Mutex::new(Talc::new(SyscallOom {
                next_chunk: FIRST_CHUNK,
            })),
        }
    }
}

// SAFETY: alloc/dealloc выдают валидные блоки и согласованно маршрутизируют
// passthrough и talc по layout.size(); talc-доступ сериализован Mutex'ом.
unsafe impl GlobalAlloc for ProcessHeap {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if layout.size() >= PASSTHROUGH_THRESHOLD {
            return passthrough_alloc(layout);
        }
        let mut talc = self.talc.lock();

        // SAFETY: GlobalAlloc гарантирует ненулевой size - единственное
        // предусловие talc.malloc; выравнивание talc обслуживает сам, доступ
        // эксклюзивен под guard'ом.
        match unsafe { talc.malloc(layout) } {
            Ok(block) => block.as_ptr(),
            Err(()) => ptr::null_mut(),
        }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        if layout.size() >= PASSTHROUGH_THRESHOLD {
            let bytes = align_up(layout.size(), PAGE_SIZE);
            let _ = memory_free(ptr as usize as u64, bytes as u64);
            return;
        }
        let Some(block) = NonNull::new(ptr) else {
            return;
        };
        let mut talc = self.talc.lock();
        // SAFETY: block получен из talc.malloc с тем же layout (маршрут
        // детерминирован по size) и ещё не освобождался.
        unsafe { talc.free(block, layout) };
    }
}

/// Выделяет блок напрямую у ядра; page-выровненный VA покрывает любой
/// `align <= PAGE_SIZE`, для более строгого выравнивания - отказ.
fn passthrough_alloc(layout: Layout) -> *mut u8 {
    if layout.align() > PAGE_SIZE {
        return ptr::null_mut();
    }
    let bytes = align_up(layout.size(), PAGE_SIZE);
    let Some(resource) = metering_handle() else {
        return ptr::null_mut();
    };
    let va = memory_allocate(resource, bytes as u64, MEM_FLAGS_READ_WRITE);
    match usize::try_from(va) {
        Ok(addr) if addr != 0 => addr as *mut u8,
        _ => ptr::null_mut(),
    }
}
