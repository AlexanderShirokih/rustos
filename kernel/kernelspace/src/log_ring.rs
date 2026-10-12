//! Кольцо `klog` в регионе, который ядро выдаёт userspace, и сигналы читателей кольца.
//!
//! Сигналы поднимает поток `bootstrap-log` после каждой строки userspace, вне лока `klog`.
//! Пробуждение берёт локи планировщика и контроллера прерываний, а код контроллера пишет в лог
//! под своим локом.

use alloc::{string::String, sync::Arc, vec::Vec};
use core::{num::NonZeroUsize, sync::atomic::AtomicU64};

use capability::{SIGNALED, Signal};
use kbase::{LockCell, MutexCell};
use klog::warn;
use memory::{AccessMask, MemoryRegion, virtual_address::PageAlignedVirtualAddress};
use spin::Once;

use crate::{kernel_context::KernelContext, syscall_bridge};

const LOG_RING_PAGES: NonZeroUsize = NonZeroUsize::new(32).unwrap();

static LOG_RING: Once<LogRing> = Once::new();

struct LogRing {
    region: Arc<MemoryRegion>,
    words: &'static [AtomicU64],
    readers: MutexCell<Vec<Arc<Signal>>>,
}

pub(crate) fn install(context: &KernelContext) {
    if LOG_RING.get().is_some() {
        return;
    }
    match map_region(context) {
        Ok((region, address)) => attach(region, address),
        Err(reason) => {
            warn!("log ring: {}", reason);
            klog::detach_ring();
        }
    }
}

fn map_region(
    context: &KernelContext,
) -> Result<(MemoryRegion, PageAlignedVirtualAddress), String> {
    let frames = syscall_bridge::frame_allocator().ok_or("no frame allocator")?;
    let region = MemoryRegion::create_virtual(frames, LOG_RING_PAGES, AccessMask::RW)
        .map_err(|_| "out of memory")?;
    let address = context
        .kernel_arena()
        .map_cached(&region)
        .map_err(|error| error.0)?;

    Ok((region, address))
}

fn attach(region: MemoryRegion, address: PageAlignedVirtualAddress) {
    let ring = LOG_RING.call_once(|| {
        let len = region.size_bytes().get() / size_of::<AtomicU64>();
        #[allow(unsafe_code)]
        // SAFETY: `region` отображён по `address`, хранится в `LOG_RING` до выключения, и арена
        // его не снимает. Слова выровнены и читаются только атомарно.
        let words =
            unsafe { core::slice::from_raw_parts(address.as_usize() as *const AtomicU64, len) };
        LogRing {
            region: Arc::new(region),
            words,
            readers: MutexCell::new(Vec::new()),
        }
    });
    klog::attach_ring(ring.words);
}

pub(crate) fn acquire() -> Option<(Arc<MemoryRegion>, Arc<Signal>)> {
    let ring = LOG_RING.get()?;
    let signal = Signal::new();

    ring.readers.with_lock(|readers| {
        readers.try_reserve(1).ok()?;
        readers.push(signal.clone());
        Some(())
    })?;

    Some((ring.region.clone(), signal))
}

pub(crate) fn notify() {
    let Some(ring) = LOG_RING.get() else {
        return;
    };

    ring.readers.with_lock(|readers| {
        readers.retain(|signal| Arc::strong_count(signal) > 1);
        for signal in readers.iter() {
            signal.signal(SIGNALED, 0);
        }
    });
}
