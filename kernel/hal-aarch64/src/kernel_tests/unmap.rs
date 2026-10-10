//! Платформенные интеграционные тесты `MemoryMapper::unmap` и
//! транзакционности `MemoryMapper::map` для user-AS.
//!
//! Каждый тест создаёт свежий user-AS, выполняет map/unmap и проверяет
//! состояние page-tables через `query_leaf_raw` (доступен только под
//! `feature = "kernel-tests"`). По завершении функции `Arc<AddressSpace>`
//! дропается - Drop возвращает все фреймы (root + intermediate L1/L2/L3
//! таблицы) во `FrameAllocator`. AS никогда не активируется в текущем CPU,
//! и в TLB его entries нет, инвалидация перед drop не нужна.

#![allow(unsafe_code)]

extern crate alloc;

use alloc::sync::Arc;
use core::{
    num::NonZeroUsize,
    sync::atomic::{AtomicIsize, AtomicUsize, Ordering},
};

use hal_aarch64_paging::{
    level::{L0, L1, L2, L3, Level},
    mapper::PageMapper,
    page_table::PageTable,
};
use kbase::MutexCell;
use kernelspace::syscall_bridge;
use memory::{
    MemFlags,
    frame::Frame,
    frame_allocator::{FrameAllocator, FrameError, ReserveFrameError},
    memory_mapper::{MemoryMapper, MemoryMappingError, MemoryUnmappingError},
    physical_address::PageAlignedAddress,
    virtual_address::PageAlignedVirtualAddress,
};
use scheduler::AddressSpace;

use super::probe_va;
use crate::{
    consts::HIGHER_HALF_BASE,
    memory::{
        address_space_factory::UserAarch64MemoryMapper,
        memory_mapper::{
            Aarch64MemoryMapper, AddressSpaceKind, FrameTableAlloc, UNMAP_PENDING_FRAMES,
        },
    },
};

fn downcast_user_mapper(mapper: &(dyn MemoryMapper + Send + Sync)) -> &UserAarch64MemoryMapper {
    mapper
        .as_any()
        .downcast_ref::<UserAarch64MemoryMapper>()
        .expect("user-AS mapper must be Aarch64MemoryMapper")
}

type RuntimeFrameAllocator = dyn FrameAllocator + Send + Sync;

type CappedMapper<'a> = Aarch64MemoryMapper<
    'a,
    CappedAllocator<'a>,
    MutexCell<PageMapper<FrameTableAlloc<'a, CappedAllocator<'a>>>>,
>;

const PAGE_SIZE: usize = 1usize << L3::SHIFT;
const L2_BLOCK_SIZE: usize = 1usize << L2::SHIFT;
const L1_BLOCK_SIZE: usize = 1usize << L1::SHIFT;

const PROBE_BASE: usize = probe_va::UNMAP.base();

const L2_BLOCK_VA: usize = probe_va::UNMAP_L2_BLOCK.base();

const L1_BLOCK_VA: usize = probe_va::UNMAP_L1_BLOCK.base();

const PARTIAL_BLOCK_VA: usize = probe_va::UNMAP_PARTIAL_BLOCK.base();

fn make_user_as() -> Arc<AddressSpace> {
    let factory =
        syscall_bridge::address_space_factory().expect("address space factory must be installed");
    AddressSpace::new_user(factory).expect("create user AS")
}

fn va(addr: usize) -> PageAlignedVirtualAddress {
    PageAlignedVirtualAddress::from_usize(addr).expect("aligned VA")
}

fn pa(addr: usize) -> PageAlignedAddress {
    PageAlignedAddress::from_usize(addr).expect("aligned PA")
}

fn runtime_frame_allocator() -> &'static RuntimeFrameAllocator {
    syscall_bridge::frame_allocator().expect("frame allocator must be installed")
}

/// `map -> unmap -> query_leaf_raw` должен показать, что leaf-PTE обнулены.
#[test_harness::test]
fn mapper_unmap_roundtrip() {
    let user_as = make_user_as();
    let mapper = user_as.mapper().expect("user variant has mapper");
    let base = va(PROBE_BASE);
    let pages = 4;

    mapper
        .map(base, pages, &[], MemFlags::user_rw())
        .expect("initial map must succeed");

    for i in 0..pages {
        let p = va(PROBE_BASE + i * PAGE_SIZE);
        test_harness::check!(downcast_user_mapper(mapper).query_leaf_raw(p).is_some());
    }

    mapper
        .unmap(base, pages * PAGE_SIZE)
        .expect("unmap must succeed");

    for i in 0..pages {
        let p = va(PROBE_BASE + i * PAGE_SIZE);
        test_harness::check!(downcast_user_mapper(mapper).query_leaf_raw(p).is_none());
    }
}

#[test_harness::test]
fn mapper_map_rejects_init_longer_than_range() {
    let user_as = make_user_as();
    let mapper = user_as.mapper().expect("user variant has mapper");
    let base = va(PROBE_BASE + 0x50_0000);
    let pages = 1;
    let init = [0xA5u8; PAGE_SIZE + 1];

    let err = mapper
        .map(base, pages, &init, MemFlags::user_rw())
        .expect_err("init longer than the range must be rejected");
    test_harness::check!(matches!(err, MemoryMappingError::InitExceedsMapping));

    test_harness::check!(downcast_user_mapper(mapper).query_leaf_raw(base).is_none());
}

#[test_harness::test]
fn mapper_map_rejects_init_for_empty_range() {
    let user_as = make_user_as();
    let mapper = user_as.mapper().expect("user variant has mapper");
    let base = va(PROBE_BASE + 0x60_0000);

    let err = mapper
        .map(base, 0, &[0xA5], MemFlags::user_rw())
        .expect_err("init for an empty range must be rejected");
    test_harness::check!(matches!(err, MemoryMappingError::InitExceedsMapping));

    test_harness::check!(downcast_user_mapper(mapper).query_leaf_raw(base).is_none());
}

#[test_harness::test]
fn mapper_map_rejects_page_count_size_overflow() {
    let user_as = make_user_as();
    let mapper = user_as.mapper().expect("user variant has mapper");
    let base = va(PROBE_BASE + 0x70_0000);

    let err = mapper
        .map(base, usize::MAX, &[], MemFlags::user_rw())
        .expect_err("mapping size overflow must be rejected");
    test_harness::check!(matches!(err, MemoryMappingError::VirtualMappingError));

    test_harness::check!(downcast_user_mapper(mapper).query_leaf_raw(base).is_none());
}

#[test_harness::test]
fn mapper_map_rejects_virtual_range_overflow() {
    let user_as = make_user_as();
    let mapper = user_as.mapper().expect("user variant has mapper");
    let base = va(usize::MAX & !(PAGE_SIZE - 1));

    let err = mapper
        .map(base, 1, &[], MemFlags::user_rw())
        .expect_err("virtual range overflow must be rejected");
    test_harness::check!(matches!(err, MemoryMappingError::VirtualMappingError));

    test_harness::check!(downcast_user_mapper(mapper).query_leaf_raw(base).is_none());
}

#[test_harness::test]
fn mapper_unmap_then_remap() {
    let user_as = make_user_as();
    let mapper = user_as.mapper().expect("user variant has mapper");
    let base = va(PROBE_BASE + 0x10_0000);
    let pages = 2;

    mapper
        .map(base, pages, &[], MemFlags::user_rw())
        .expect("initial map");

    mapper
        .unmap(base, pages * PAGE_SIZE)
        .expect("unmap must succeed");

    mapper
        .map(base, pages, &[], MemFlags::user_ro())
        .expect("re-map after unmap must succeed");

    for i in 0..pages {
        test_harness::check!(
            downcast_user_mapper(mapper)
                .query_leaf_raw(va(base.as_usize() + i * PAGE_SIZE))
                .is_some()
        );
    }

    // Очищаем, чтобы Drop AS не упал на остаточном маппинге, когда у него
    // появится sanity-check.
    mapper
        .unmap(base, pages * PAGE_SIZE)
        .expect("cleanup unmap");
}

/// `unmap` несуществующего диапазона возвращает `NotMapped`.
#[test_harness::test]
fn mapper_unmap_unmapped_returns_not_mapped() {
    let user_as = make_user_as();
    let mapper = user_as.mapper().expect("user variant has mapper");
    let base = va(PROBE_BASE + 0x20_0000);

    let err = mapper
        .unmap(base, PAGE_SIZE)
        .expect_err("unmap of unmapped VA must fail");
    test_harness::check!(matches!(err, MemoryUnmappingError::NotMapped));
}

/// Регресс к Codex P1: до появления `OWNED_BY_FA_BIT` `unmap` дёргал
/// `deallocate_frame` на любом leaf и корруптил bitmap на cleanup MMIO.
#[test_harness::test]
fn mapper_unmap_after_map_exact_keeps_frame_allocated() {
    let real_fa = runtime_frame_allocator();
    let user_as = make_user_as();
    let mapper = user_as.mapper().expect("user variant has mapper");

    // Sentinel-фрейм через настоящий FA: PA заведомо лежит в RAM-bitmap и
    // помечен как `is_allocated`. Имитирует caller-supplied PA, который mapper
    // в FA не возвращает.
    let sentinel = real_fa.allocate_frame().expect("sentinel frame");
    let sentinel_pa = sentinel.page_address();
    test_harness::check!(real_fa.is_allocated(sentinel));

    let target_va = va(PROBE_BASE + 0x50_0000);
    mapper
        .map_exact(target_va, sentinel_pa, PAGE_SIZE, MemFlags::user_rw())
        .expect("map_exact sentinel");

    mapper.unmap(target_va, PAGE_SIZE).expect("unmap sentinel");

    // Главный assert: после unmap sentinel-фрейм всё ещё считается занятым.
    test_harness::check!(real_fa.is_allocated(sentinel));

    // Sentinel освобождаем сами - фрейм наш, mapper его не выдавал.
    let _ = real_fa.deallocate_frame(sentinel);
}

fn build_capped_mapper() -> (
    &'static CappedMapper<'static>,
    &'static CappedAllocator<'static>,
    Frame,
) {
    let real_fa = runtime_frame_allocator();
    let root_frame = real_fa.allocate_frame().expect("root frame");
    let root_ptr = (root_frame.page_address().as_usize() + HIGHER_HALF_BASE) as *mut PageTable<L0>;
    // SAFETY: свежевыделенный фрейм через higher-half-карту.
    unsafe { root_ptr.write(PageTable::<L0>::new()) };

    let capped: &'static CappedAllocator<'static> = alloc::boxed::Box::leak(
        alloc::boxed::Box::new(CappedAllocator::new(real_fa, isize::MAX)),
    );
    let mapper: &'static CappedMapper<'static> = alloc::boxed::Box::leak(alloc::boxed::Box::new(
        Aarch64MemoryMapper::new_with_offset(
            capped,
            root_ptr,
            HIGHER_HALF_BASE,
            AddressSpaceKind::User,
        ),
    ));
    (mapper, capped, root_frame)
}

/// `unmap` последней 4K leaf-записи возвращает leaf и все промежуточные L1/L2/L3.
#[test_harness::test]
fn mapper_unmap_releases_empty_tables() {
    let (mapper, capped, root_frame) = build_capped_mapper();
    let baseline = capped.outstanding();

    let base = va(PROBE_BASE + 0x60_0000);
    mapper
        .map(base, 1, &[], MemFlags::user_rw())
        .expect("initial map");
    // leaf + L1 + L2 + L3 = 4.
    test_harness::check!(capped.outstanding() == baseline + 4);

    mapper.unmap(base, PAGE_SIZE).expect("unmap last page");
    test_harness::check!(capped.outstanding() == baseline);

    let _ = runtime_frame_allocator().deallocate_frame(root_frame);
}

/// Снятие диапазона, который переполняет буфер отложенных фреймов, возвращает их все.
#[test_harness::test]
fn mapper_unmap_releases_frames_past_pending_buffer() {
    let (mapper, capped, root_frame) = build_capped_mapper();
    let baseline = capped.outstanding();

    let base = va(PROBE_BASE + 0x70_0000);
    let pages = UNMAP_PENDING_FRAMES * 2 + 1;
    mapper
        .map(base, pages, &[], MemFlags::user_rw())
        .expect("initial map");
    mapper
        .unmap(base, pages * PAGE_SIZE)
        .expect("unmap the whole range");
    test_harness::check!(capped.outstanding() == baseline);

    let _ = runtime_frame_allocator().deallocate_frame(root_frame);
}

/// Partial unmap внутри L2 block-leaf отбивается без модификации таблиц.
#[test_harness::test]
fn mapper_unmap_partial_block_rejected() {
    let (mapper, capped, root_frame) = build_capped_mapper();

    let base = va(PARTIAL_BLOCK_VA);
    let phys = pa(0x6000_0000);
    mapper
        .map_exact(base, phys, L2_BLOCK_SIZE, MemFlags::user_rw())
        .expect("map_exact 2M block");
    let after_map = capped.outstanding();

    // 4К внутри 2М-блока.
    let inside = va(base.as_usize() + PAGE_SIZE);
    let err = mapper
        .unmap(inside, PAGE_SIZE)
        .expect_err("partial unmap inside block must reject");
    test_harness::check!(matches!(err, MemoryUnmappingError::UnsupportedBlockMapping));
    test_harness::check!(capped.outstanding() == after_map);

    // Aligned start, но size < block.
    let err = mapper
        .unmap(base, PAGE_SIZE)
        .expect_err("aligned-start short unmap inside block must reject");
    test_harness::check!(matches!(err, MemoryUnmappingError::UnsupportedBlockMapping));
    test_harness::check!(capped.outstanding() == after_map);

    mapper
        .unmap(base, L2_BLOCK_SIZE)
        .expect("whole-block unmap must succeed after partial reject");

    let _ = runtime_frame_allocator().deallocate_frame(root_frame);
}

/// `map_exact` может поставить L2 block descriptor. Exact unmap всего блока
/// должен снять его без split и разрешить повторный map_exact.
#[test_harness::test]
fn mapper_unmap_l2_block_then_remap() {
    let user_as = make_user_as();
    let mapper = user_as.mapper().expect("user variant has mapper");
    let base = va(L2_BLOCK_VA);
    let phys = pa(0x2000_0000);

    mapper
        .map_exact(base, phys, L2_BLOCK_SIZE, MemFlags::user_rw())
        .expect("map_exact 2M block");

    mapper
        .unmap(base, L2_BLOCK_SIZE)
        .expect("unmap exact 2M block");

    mapper
        .map_exact(base, phys, L2_BLOCK_SIZE, MemFlags::user_ro())
        .expect("re-map after 2M block unmap");

    mapper.unmap(base, L2_BLOCK_SIZE).expect("cleanup 2M block");
}

/// Аналогично для L1 block descriptor: exact 1G block unmap снимает leaf без
/// выделения/split-а нижних таблиц.
#[test_harness::test]
fn mapper_unmap_l1_block_then_remap() {
    let user_as = make_user_as();
    let mapper = user_as.mapper().expect("user variant has mapper");
    let base = va(L1_BLOCK_VA);
    let phys = pa(0x4000_0000);

    mapper
        .map_exact(base, phys, L1_BLOCK_SIZE, MemFlags::user_rw())
        .expect("map_exact 1G block");

    mapper
        .unmap(base, L1_BLOCK_SIZE)
        .expect("unmap exact 1G block");

    mapper
        .map_exact(base, phys, L1_BLOCK_SIZE, MemFlags::user_ro())
        .expect("re-map after 1G block unmap");

    mapper.unmap(base, L1_BLOCK_SIZE).expect("cleanup 1G block");
}

/// `unmap` с size, не кратным странице, отбивается типом ошибки до любых
/// модификаций таблиц.
#[test_harness::test]
fn mapper_unmap_misaligned_size_rejected() {
    let user_as = make_user_as();
    let mapper = user_as.mapper().expect("user variant has mapper");
    let base = va(PROBE_BASE + 0x30_0000);

    mapper
        .map(base, 1, &[], MemFlags::user_rw())
        .expect("initial map");

    let err = mapper
        .unmap(base, PAGE_SIZE + 1)
        .expect_err("unmap must reject misaligned size");
    test_harness::check!(matches!(err, MemoryUnmappingError::MisalignedRange));

    test_harness::check!(downcast_user_mapper(mapper).query_leaf_raw(base).is_some());

    mapper.unmap(base, PAGE_SIZE).expect("cleanup unmap");
}

struct CappedAllocator<'a> {
    inner: &'a RuntimeFrameAllocator,
    remaining: AtomicIsize,
    outstanding: AtomicUsize,
}

impl<'a> CappedAllocator<'a> {
    fn new(inner: &'a RuntimeFrameAllocator, limit: isize) -> Self {
        Self {
            inner,
            remaining: AtomicIsize::new(limit),
            outstanding: AtomicUsize::new(0),
        }
    }
    fn outstanding(&self) -> usize {
        self.outstanding.load(Ordering::SeqCst)
    }
}

impl FrameAllocator for CappedAllocator<'_> {
    fn reserve_frames_exact(&self, from: Frame, to: Frame) -> Result<Frame, ReserveFrameError> {
        self.inner.reserve_frames_exact(from, to)
    }
    fn allocate_frame(&self) -> Option<Frame> {
        if self.remaining.fetch_sub(1, Ordering::SeqCst) <= 0 {
            return None;
        }
        let frame = self.inner.allocate_frame()?;
        self.outstanding.fetch_add(1, Ordering::SeqCst);
        Some(frame)
    }
    fn allocate_frames(&self, count: NonZeroUsize) -> Option<Frame> {
        // На пути `Aarch64MemoryMapper::map` не вызывается, делегируем без декремента.
        self.inner.allocate_frames(count)
    }
    fn deallocate_frame(&self, frame: Frame) -> Result<(), FrameError> {
        let r = self.inner.deallocate_frame(frame);
        if r.is_ok() {
            self.outstanding.fetch_sub(1, Ordering::SeqCst);
        }
        r
    }
    fn is_allocated(&self, frame: Frame) -> bool {
        self.inner.is_allocated(frame)
    }
}

#[test_harness::test]
fn mapper_map_partial_oom_rollback() {
    let real_fa = runtime_frame_allocator();

    // Root-фрейм идём через настоящий FA - обёртка ниже считает только то,
    // что аллоцирует сам mapper в `map`.
    let root_frame = real_fa.allocate_frame().expect("root frame");
    let root_pa = root_frame.page_address();
    let root_ptr = (root_pa.as_usize() + HIGHER_HALF_BASE) as *mut PageTable<L0>;
    // SAFETY: свежевыделенный фрейм; mapping PA->VA через higher-half линейную
    // карту валиден после MMU; запись пустой PageTable инициализирует в Invalid.
    unsafe { root_ptr.write(PageTable::<L0>::new()) };

    // 4 страницы подряд в одном L1/L2/L3-trio: первая страница требует
    // 3 intermediate-таблицы + 1 leaf-фрейм; страницы 2-4 - по 1 leaf-фрейму
    // (intermediate уже есть). Итого 7 запросов. Лимит 5 -> fail на 6-м
    // (3-я leaf-страница), rollback откатит leaf-страницы 0..2.
    //
    // `Box::leak` нужен, чтобы поднять lifetime обёртки до `'static` -
    // `MemoryMapper::as_any` требует `Self: 'static`, иначе trait-impl на
    // `Aarch64MemoryMapper<'a, ...>` не подходит для `dyn Any`. В QEMU-runner-е
    // утечка приемлема. Тест-сюита не возвращается, а аллокатор остаётся
    // валидным после выхода через semihosting.
    let capped: &'static CappedAllocator<'static> =
        alloc::boxed::Box::leak(alloc::boxed::Box::new(CappedAllocator::new(real_fa, 5)));

    let mapper: CappedMapper<'static> = Aarch64MemoryMapper::new_with_offset(
        capped,
        root_ptr,
        HIGHER_HALF_BASE,
        AddressSpaceKind::User,
    );

    let base = va(PROBE_BASE + 0x40_0000);
    let pages = 4;
    let err = mapper
        .map(base, pages, &[], MemFlags::user_rw())
        .expect_err("map must OOM after capped allocate count");
    test_harness::check!(matches!(err, MemoryMappingError::OutOfMemory));

    for i in 0..pages {
        let p = va(base.as_usize() + i * PAGE_SIZE);
        test_harness::check!(mapper.query_leaf_raw(p).is_none());
    }
    // Откат возвращает и leaf-фреймы, и опустевшие L1/L2/L3.
    test_harness::check!(capped.outstanding() == 0);
}
