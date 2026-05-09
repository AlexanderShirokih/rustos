//! Платформенные интеграционные тесты `MemoryMapper::unmap` и
//! транзакционности `MemoryMapper::map` для user-AS.
//!
//! Каждый тест создаёт свежий user-AS, выполняет map/unmap и проверяет
//! состояние page-tables через `query_leaf_raw` (доступен только под
//! `feature = "qemu-tests"`). По завершении функции `Arc<AddressSpace>`
//! дропается - Drop возвращает все фреймы (root + intermediate L1/L2/L3
//! таблицы) во `FrameAllocator`. AS никогда не активируется в текущем CPU,
//! поэтому в TLB его entries нет и инвалидация перед drop'ом не нужна.

#![allow(unsafe_code)]

extern crate alloc;

use alloc::sync::Arc;
use core::sync::atomic::{AtomicIsize, AtomicUsize, Ordering};

use collections::MutexCell;
use hal_aarch64_paging::{
    level::{L0, L1, L2, L3, Level},
    mapper::PageMapper,
    page_table::PageTable,
};
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
use test_harness_qemu::register_test;

use crate::{
    HIGHER_HALF_BASE,
    memory::{
        address_space_factory::{
            FrameAllocatorImpl, UserAarch64MemoryMapper, qemu_test_frame_allocator,
        },
        memory_mapper::{Aarch64MemoryMapper, AddressSpaceKind, FrameTableAlloc},
    },
};

/// Downcast'ит `&dyn MemoryMapper`, выданный `Aarch64AddressSpaceFactory`,
/// до конкретного типа для доступа к платформенному API диагностики.
fn downcast_user_mapper(mapper: &(dyn MemoryMapper + Send + Sync)) -> &UserAarch64MemoryMapper {
    mapper
        .as_any()
        .downcast_ref::<UserAarch64MemoryMapper>()
        .expect("user-AS mapper must be Aarch64MemoryMapper")
}

/// Mapper-тип под `CappedAllocator` с `MutexCell`-стратегией -
/// та же, что и у production-фабрики user-AS.
type CappedMapper<'a> = Aarch64MemoryMapper<
    'a,
    CappedAllocator<'a, FrameAllocatorImpl>,
    MutexCell<PageMapper<FrameTableAlloc<'a, CappedAllocator<'a, FrameAllocatorImpl>>>>,
>;

const PAGE_SIZE: usize = 1usize << L3::SHIFT;
const L2_BLOCK_SIZE: usize = 1usize << L2::SHIFT;
const L1_BLOCK_SIZE: usize = 1usize << L1::SHIFT;

/// Lower-half VA, не пересекающаяся с другими user-AS-тестами.
const PROBE_BASE: usize = 0x6000_0000;

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

/// `map -> unmap -> query_leaf_raw` должен показать, что leaf-PTE обнулены.
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
        test_harness_qemu::kassert!(downcast_user_mapper(mapper).query_leaf_raw(p).is_some());
    }

    mapper
        .unmap(base, pages * PAGE_SIZE)
        .expect("unmap must succeed");

    for i in 0..pages {
        let p = va(PROBE_BASE + i * PAGE_SIZE);
        test_harness_qemu::kassert!(downcast_user_mapper(mapper).query_leaf_raw(p).is_none());
    }
}

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
        test_harness_qemu::kassert!(
            downcast_user_mapper(mapper)
                .query_leaf_raw(va(base.as_usize() + i * PAGE_SIZE))
                .is_some()
        );
    }

    // Очищаем, чтобы Drop AS не упал на остаточном маппинге, если в Drop'е
    // когда-нибудь появится sanity-check.
    mapper
        .unmap(base, pages * PAGE_SIZE)
        .expect("cleanup unmap");
}

/// `unmap` несуществующего диапазона возвращает `NotMapped`.
fn mapper_unmap_unmapped_returns_not_mapped() {
    let user_as = make_user_as();
    let mapper = user_as.mapper().expect("user variant has mapper");
    let base = va(PROBE_BASE + 0x20_0000);

    let err = mapper
        .unmap(base, PAGE_SIZE)
        .expect_err("unmap of unmapped VA must fail");
    test_harness_qemu::kassert!(matches!(err, MemoryUnmappingError::NotMapped));
}

/// `unmap` после `map_exact` НЕ возвращает PA в `FrameAllocator` - для
/// caller-supplied маппингов (MMIO, image, identity) PA не принадлежит
/// RAM-bitmap'у. Регресс-тест к Codex P1: до фикса OWNED_BY_FA_BIT
/// `unmap` безусловно дёргал `deallocate_frame` для любого leaf'а и
/// корруптил bitmap при cleanup MMIO-маппинга.
fn mapper_unmap_after_map_exact_keeps_frame_allocated() {
    let real_fa = qemu_test_frame_allocator();
    let user_as = make_user_as();
    let mapper = user_as.mapper().expect("user variant has mapper");

    // Sentinel-фрейм через настоящий FA: PA заведомо в RAM-bitmap'е и помечен
    // как `is_allocated`. Имитирует caller-supplied PA, который mapper не
    // должен возвращать в FA.
    let sentinel = real_fa.allocate_frame().expect("sentinel frame");
    let sentinel_pa = sentinel.page_address();
    test_harness_qemu::kassert!(real_fa.is_allocated(sentinel));

    let target_va = va(PROBE_BASE + 0x50_0000);
    mapper
        .map_exact(target_va, sentinel_pa, PAGE_SIZE, MemFlags::user_rw())
        .expect("map_exact sentinel");

    mapper.unmap(target_va, PAGE_SIZE).expect("unmap sentinel");

    // Главный assert: после unmap sentinel-фрейм всё ещё считается занятым.
    test_harness_qemu::kassert!(real_fa.is_allocated(sentinel));

    // Sentinel освобождаем сами - фрейм был наш, не mapper'а.
    let _ = real_fa.deallocate_frame(sentinel);
}

/// Создаёт mapper над `CappedAllocator` для тестов, замеряющих outstanding-счётчик.
/// Возвращает `(mapper, capped, root_frame)` - root-фрейм должен быть вручную
/// возвращён в FA после теста, чтобы не фрагментировать bitmap (Box::leak'нутый
/// mapper никогда не дропается, а его DropPath срабатывает только если
/// `had_mappings == true`).
fn build_capped_mapper() -> (
    &'static CappedMapper<'static>,
    &'static CappedAllocator<'static, FrameAllocatorImpl>,
    Frame,
) {
    let real_fa: &'static FrameAllocatorImpl = qemu_test_frame_allocator();
    let root_frame = real_fa.allocate_frame().expect("root frame");
    let root_ptr = (root_frame.page_address().as_usize() + HIGHER_HALF_BASE) as *mut PageTable<L0>;
    // SAFETY: свежевыделенный фрейм через higher-half-карту.
    unsafe { root_ptr.write(PageTable::<L0>::new()) };

    let capped: &'static CappedAllocator<'static, FrameAllocatorImpl> = alloc::boxed::Box::leak(
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
fn mapper_unmap_releases_empty_tables() {
    let (mapper, capped, root_frame) = build_capped_mapper();
    let baseline = capped.outstanding();

    let base = va(PROBE_BASE + 0x60_0000);
    mapper
        .map(base, 1, &[], MemFlags::user_rw())
        .expect("initial map");
    // leaf + L1 + L2 + L3 = 4.
    test_harness_qemu::kassert!(capped.outstanding() == baseline + 4);

    mapper.unmap(base, PAGE_SIZE).expect("unmap last page");
    test_harness_qemu::kassert!(capped.outstanding() == baseline);

    let _ = qemu_test_frame_allocator().deallocate_frame(root_frame);
}

/// Partial unmap внутри L2 block-leaf отбивается без модификации таблиц.
fn mapper_unmap_partial_block_rejected() {
    let (mapper, capped, root_frame) = build_capped_mapper();

    let base = va(0x9000_0000);
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
    test_harness_qemu::kassert!(matches!(err, MemoryUnmappingError::UnsupportedBlockMapping));
    test_harness_qemu::kassert!(capped.outstanding() == after_map);

    // Aligned start, но size < block.
    let err = mapper
        .unmap(base, PAGE_SIZE)
        .expect_err("aligned-start short unmap inside block must reject");
    test_harness_qemu::kassert!(matches!(err, MemoryUnmappingError::UnsupportedBlockMapping));
    test_harness_qemu::kassert!(capped.outstanding() == after_map);

    mapper
        .unmap(base, L2_BLOCK_SIZE)
        .expect("whole-block unmap must succeed after partial reject");

    let _ = qemu_test_frame_allocator().deallocate_frame(root_frame);
}

/// `map_exact` может поставить L2 block descriptor; exact unmap всего блока
/// должен снять его без split и разрешить повторный map_exact.
fn mapper_unmap_l2_block_then_remap() {
    let user_as = make_user_as();
    let mapper = user_as.mapper().expect("user variant has mapper");
    let base = va(0x7000_0000);
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
fn mapper_unmap_l1_block_then_remap() {
    let user_as = make_user_as();
    let mapper = user_as.mapper().expect("user variant has mapper");
    let base = va(0x8000_0000);
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
    test_harness_qemu::kassert!(matches!(err, MemoryUnmappingError::MisalignedRange));

    test_harness_qemu::kassert!(downcast_user_mapper(mapper).query_leaf_raw(base).is_some());

    mapper.unmap(base, PAGE_SIZE).expect("cleanup unmap");
}

/// Обёртка над настоящим `FrameAllocator` с лимитом на allocate'ы и
/// счётчиком outstanding (allocated - deallocated). `outstanding == 0`
/// после `map`/`unmap` означает, что mapper честно вернул все фреймы.
struct CappedAllocator<'a, FA: FrameAllocator> {
    inner: &'a FA,
    remaining: AtomicIsize,
    outstanding: AtomicUsize,
}

impl<'a, FA: FrameAllocator> CappedAllocator<'a, FA> {
    fn new(inner: &'a FA, limit: isize) -> Self {
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

impl<FA: FrameAllocator> FrameAllocator for CappedAllocator<'_, FA> {
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
    fn allocate_frames(&self, max_count: usize) -> Option<(Frame, usize)> {
        // На пути `Aarch64MemoryMapper::map` не вызывается, делегируем без декремента.
        self.inner.allocate_frames(max_count)
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

/// `map(VA, 4)` при OOM на середине цикла откатывает все успешно
/// замапленные leaf-страницы. После Err - `query_leaf_raw` всех страниц
/// диапазона возвращает `None`.
fn mapper_map_partial_oom_rollback() {
    let real_fa: &'static FrameAllocatorImpl = qemu_test_frame_allocator();

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
    // утечка приемлема: тест-сюита не возвращается, а аллокатор переживёт
    // выход через semihosting.
    let capped: &'static CappedAllocator<'static, FrameAllocatorImpl> =
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
    test_harness_qemu::kassert!(matches!(err, MemoryMappingError::OutOfMemory));

    for i in 0..pages {
        let p = va(base.as_usize() + i * PAGE_SIZE);
        test_harness_qemu::kassert!(mapper.query_leaf_raw(p).is_none());
    }

    // Drop mapper'а вернёт root + intermediate L1/L2/L3 в реальный FA через
    // делегирование `deallocate_frame`. Leaf-фреймов 0..2 уже нет в дереве -
    // они откачены rollback'ом до возврата ошибки.
}

register_test!(
    MAPPER_UNMAP_ROUNDTRIP,
    "mapper_unmap_roundtrip",
    mapper_unmap_roundtrip
);
register_test!(
    MAPPER_UNMAP_THEN_REMAP,
    "mapper_unmap_then_remap",
    mapper_unmap_then_remap
);
register_test!(
    MAPPER_UNMAP_UNMAPPED_RETURNS_NOT_MAPPED,
    "mapper_unmap_unmapped_returns_not_mapped",
    mapper_unmap_unmapped_returns_not_mapped
);
register_test!(
    MAPPER_UNMAP_MISALIGNED_SIZE,
    "mapper_unmap_misaligned_size_rejected",
    mapper_unmap_misaligned_size_rejected
);
register_test!(
    MAPPER_UNMAP_AFTER_MAP_EXACT_KEEPS_FRAME,
    "mapper_unmap_after_map_exact_keeps_frame_allocated",
    mapper_unmap_after_map_exact_keeps_frame_allocated
);
register_test!(
    MAPPER_UNMAP_RELEASES_EMPTY_TABLES,
    "mapper_unmap_releases_empty_tables",
    mapper_unmap_releases_empty_tables
);
register_test!(
    MAPPER_UNMAP_PARTIAL_BLOCK_REJECTED,
    "mapper_unmap_partial_block_rejected",
    mapper_unmap_partial_block_rejected
);
register_test!(
    MAPPER_UNMAP_L2_BLOCK_THEN_REMAP,
    "mapper_unmap_l2_block_then_remap",
    mapper_unmap_l2_block_then_remap
);
register_test!(
    MAPPER_UNMAP_L1_BLOCK_THEN_REMAP,
    "mapper_unmap_l1_block_then_remap",
    mapper_unmap_l1_block_then_remap
);
register_test!(
    MAPPER_MAP_PARTIAL_OOM_ROLLBACK,
    "mapper_map_partial_oom_rollback",
    mapper_map_partial_oom_rollback
);
