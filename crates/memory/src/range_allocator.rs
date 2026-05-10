//! Generic-аллокатор переменных диапазонов виртуальных адресов.
//!
//! Управляет полуоткрытой ареной `[arena_start, arena_end)`, выдавая
//! постранично выровненные диапазоны произвольной длины. Освобождённые
//! диапазоны возвращаются обратно в пул и автоматически сливаются с
//! соседними свободными - фрагментация ограничена сверху количеством
//! живых регионов.

use alloc::vec::Vec;
use core::{
    fmt::{Display, Formatter},
    num::NonZeroUsize,
};

use crate::{
    aligned::Aligned,
    virtual_address::{PageAlignedVirtualAddress, VirtualAddress},
};

const PAGE_SIZE: usize = PageAlignedVirtualAddress::ALIGNMENT;

/// Дефолтный лимит одновременно живых регионов в реестре.
pub const DEFAULT_LIVE_REGION_CAPACITY: usize = 64;

/// Стартовый резерв покрывает обычный сценарий с единицами живых регионов,
/// не выделяя память под весь верхний лимит заранее.
const DEFAULT_PREALLOCATED_LIVE_REGIONS: usize = 4;
const DEFAULT_PREALLOCATED_FREE_RANGES: usize = DEFAULT_PREALLOCATED_LIVE_REGIONS + 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AllocateError {
    NotEnoughSpace,
    UnalignedSize,
    OutOfSlots,
}

impl Display for AllocateError {
    fn fmt(&self, f: &mut Formatter<'_>) -> core::fmt::Result {
        match self {
            AllocateError::NotEnoughSpace => f.write_str("Range allocator arena exhausted"),
            AllocateError::UnalignedSize => f.write_str("Allocation size must be page-aligned"),
            AllocateError::OutOfSlots => f.write_str("Range allocator slot table is full"),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RangeError {
    NotFound,
    UnalignedSize,
}

impl Display for RangeError {
    fn fmt(&self, f: &mut Formatter<'_>) -> core::fmt::Result {
        match self {
            RangeError::NotFound => f.write_str("No allocated range matches the request"),
            RangeError::UnalignedSize => f.write_str("Range size must be page-aligned"),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AllocatedRange<Tag: Clone> {
    base: PageAlignedVirtualAddress,
    pages: NonZeroUsize,
    tag: Tag,
}

impl<Tag: Clone> AllocatedRange<Tag> {
    pub const fn base(&self) -> PageAlignedVirtualAddress {
        self.base
    }

    pub const fn pages(&self) -> NonZeroUsize {
        self.pages
    }

    pub const fn size_bytes(&self) -> usize {
        self.pages.get() * PAGE_SIZE
    }

    pub const fn end_va(&self) -> VirtualAddress {
        VirtualAddress::new(self.base.as_usize() + self.pages.get() * PAGE_SIZE)
    }

    pub fn tag(&self) -> &Tag {
        &self.tag
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FreeRange {
    base: PageAlignedVirtualAddress,
    pages: NonZeroUsize,
}

impl FreeRange {
    pub const fn base(&self) -> PageAlignedVirtualAddress {
        self.base
    }

    pub const fn pages(&self) -> NonZeroUsize {
        self.pages
    }

    pub const fn end_va(&self) -> VirtualAddress {
        VirtualAddress::new(self.base.as_usize() + self.pages.get() * PAGE_SIZE)
    }
}

/// First-fit аллокатор VA-диапазонов с честным `free` и автоматическим
/// слиянием соседних свободных областей.
pub struct RangeAllocator<Tag: Clone> {
    arena_start: PageAlignedVirtualAddress,
    arena_end: VirtualAddress,
    free_ranges: Vec<FreeRange>,
    allocated: Vec<AllocatedRange<Tag>>,
    capacity: usize,
}

impl<Tag: Clone> RangeAllocator<Tag> {
    pub fn new(start: PageAlignedVirtualAddress, end: VirtualAddress) -> Self {
        Self::with_capacity(start, end, DEFAULT_LIVE_REGION_CAPACITY)
    }

    pub fn with_capacity(
        start: PageAlignedVirtualAddress,
        end: VirtualAddress,
        capacity: usize,
    ) -> Self {
        let has_initial_free_range = end.as_usize() > start.as_usize();
        let mut free_ranges = Vec::with_capacity(Self::preallocated_free_ranges(
            capacity,
            has_initial_free_range,
        ));
        if has_initial_free_range {
            let pages = (end.as_usize() - start.as_usize()) / PAGE_SIZE;
            if let Some(pages) = NonZeroUsize::new(pages) {
                free_ranges.push(FreeRange { base: start, pages });
            }
        }
        Self {
            arena_start: start,
            arena_end: end,
            free_ranges,
            allocated: Vec::with_capacity(Self::preallocated_live_regions(capacity)),
            capacity,
        }
    }

    const fn preallocated_live_regions(capacity: usize) -> usize {
        if capacity < DEFAULT_PREALLOCATED_LIVE_REGIONS {
            capacity
        } else {
            DEFAULT_PREALLOCATED_LIVE_REGIONS
        }
    }

    const fn preallocated_free_ranges(capacity: usize, has_initial_free_range: bool) -> usize {
        if !has_initial_free_range {
            return 0;
        }

        let max_free_ranges = capacity.saturating_add(1);
        if max_free_ranges < DEFAULT_PREALLOCATED_FREE_RANGES {
            max_free_ranges
        } else {
            DEFAULT_PREALLOCATED_FREE_RANGES
        }
    }

    pub fn arena_start(&self) -> PageAlignedVirtualAddress {
        self.arena_start
    }

    pub fn arena_end(&self) -> VirtualAddress {
        self.arena_end
    }

    pub fn live_count(&self) -> usize {
        self.allocated.len()
    }

    pub fn allocated(&self) -> &[AllocatedRange<Tag>] {
        &self.allocated
    }

    pub fn free_ranges(&self) -> &[FreeRange] {
        &self.free_ranges
    }

    pub fn allocate(
        &mut self,
        size_bytes: NonZeroUsize,
        tag: Tag,
    ) -> Result<AllocatedRange<Tag>, AllocateError> {
        if !size_bytes.get().is_multiple_of(PAGE_SIZE) {
            return Err(AllocateError::UnalignedSize);
        }
        if self.allocated.len() >= self.capacity {
            return Err(AllocateError::OutOfSlots);
        }
        let requested_pages = NonZeroUsize::new(size_bytes.get() / PAGE_SIZE)
            .expect("size_bytes >= PAGE_SIZE after alignment check");

        let (idx, picked_base) = self
            .free_ranges
            .iter()
            .enumerate()
            .find_map(|(i, r)| (r.pages.get() >= requested_pages.get()).then_some((i, r.base)))
            .ok_or(AllocateError::NotEnoughSpace)?;

        let free = &mut self.free_ranges[idx];
        if free.pages == requested_pages {
            self.free_ranges.remove(idx);
        } else {
            // Сдвигаем свободный хвост вправо - сортировка сохраняется без resort-а.
            let new_base_usize = free.base.as_usize() + requested_pages.get() * PAGE_SIZE;
            free.base = PageAlignedVirtualAddress::from_usize(new_base_usize)
                .expect("base shifted by page-multiple stays page-aligned");
            free.pages = NonZeroUsize::new(free.pages.get() - requested_pages.get())
                .expect("free.pages > requested_pages here");
        }

        let allocated = AllocatedRange {
            base: picked_base,
            pages: requested_pages,
            tag,
        };
        let returned = allocated.clone();
        let pos = self
            .allocated
            .binary_search_by_key(&picked_base.as_usize(), |r| r.base.as_usize())
            .expect_err("free range was disjoint from any allocated range");
        self.allocated.insert(pos, allocated);
        Ok(returned)
    }

    pub fn free(
        &mut self,
        base: PageAlignedVirtualAddress,
        size_bytes: NonZeroUsize,
    ) -> Result<AllocatedRange<Tag>, RangeError> {
        if !size_bytes.get().is_multiple_of(PAGE_SIZE) {
            return Err(RangeError::UnalignedSize);
        }
        let requested_pages = NonZeroUsize::new(size_bytes.get() / PAGE_SIZE)
            .expect("size_bytes >= PAGE_SIZE after alignment check");

        let idx = self
            .allocated
            .binary_search_by_key(&base.as_usize(), |r| r.base.as_usize())
            .map_err(|_| RangeError::NotFound)?;
        if self.allocated[idx].pages != requested_pages {
            return Err(RangeError::NotFound);
        }
        let removed = self.allocated.remove(idx);
        self.insert_free(removed.base, removed.pages);
        Ok(removed)
    }

    pub fn lookup(
        &self,
        base: PageAlignedVirtualAddress,
        size_bytes: NonZeroUsize,
    ) -> Result<&AllocatedRange<Tag>, RangeError> {
        if !size_bytes.get().is_multiple_of(PAGE_SIZE) {
            return Err(RangeError::UnalignedSize);
        }
        let requested_pages = NonZeroUsize::new(size_bytes.get() / PAGE_SIZE)
            .expect("size_bytes >= PAGE_SIZE after alignment check");
        let idx = self
            .allocated
            .binary_search_by_key(&base.as_usize(), |r| r.base.as_usize())
            .map_err(|_| RangeError::NotFound)?;
        let region = &self.allocated[idx];
        if region.pages != requested_pages {
            return Err(RangeError::NotFound);
        }
        Ok(region)
    }

    pub fn set_tag(
        &mut self,
        base: PageAlignedVirtualAddress,
        size_bytes: NonZeroUsize,
        new_tag: Tag,
    ) -> Result<AllocatedRange<Tag>, RangeError> {
        if !size_bytes.get().is_multiple_of(PAGE_SIZE) {
            return Err(RangeError::UnalignedSize);
        }
        let requested_pages = NonZeroUsize::new(size_bytes.get() / PAGE_SIZE)
            .expect("size_bytes >= PAGE_SIZE after alignment check");
        let idx = self
            .allocated
            .binary_search_by_key(&base.as_usize(), |r| r.base.as_usize())
            .map_err(|_| RangeError::NotFound)?;
        let region = &mut self.allocated[idx];
        if region.pages != requested_pages {
            return Err(RangeError::NotFound);
        }
        region.tag = new_tag;
        Ok(region.clone())
    }

    fn insert_free(&mut self, base: PageAlignedVirtualAddress, pages: NonZeroUsize) {
        let base_usize = base.as_usize();
        let end_usize = base_usize + pages.get() * PAGE_SIZE;
        let pos = self
            .free_ranges
            .partition_point(|r| r.base.as_usize() < base_usize);

        let touches_left = pos > 0 && self.free_ranges[pos - 1].end_va().as_usize() == base_usize;
        let touches_right =
            pos < self.free_ranges.len() && self.free_ranges[pos].base.as_usize() == end_usize;

        match (touches_left, touches_right) {
            (true, true) => {
                let right_pages = self.free_ranges[pos].pages.get();
                let left = &mut self.free_ranges[pos - 1];
                left.pages = NonZeroUsize::new(left.pages.get() + pages.get() + right_pages)
                    .expect("sum of positive pages is positive");
                self.free_ranges.remove(pos);
            }
            (true, false) => {
                let left = &mut self.free_ranges[pos - 1];
                left.pages = NonZeroUsize::new(left.pages.get() + pages.get())
                    .expect("sum of positive pages is positive");
            }
            (false, true) => {
                let right = &mut self.free_ranges[pos];
                right.base = base;
                right.pages = NonZeroUsize::new(right.pages.get() + pages.get())
                    .expect("sum of positive pages is positive");
            }
            (false, false) => {
                self.free_ranges.insert(pos, FreeRange { base, pages });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const REGION_START: usize = 0x4000_0000;
    const REGION_END: usize = 0x8000_0000;

    fn allocator() -> RangeAllocator<u32> {
        RangeAllocator::new(
            PageAlignedVirtualAddress::from_usize(REGION_START).unwrap(),
            VirtualAddress::new(REGION_END),
        )
    }

    fn nz(value: usize) -> NonZeroUsize {
        NonZeroUsize::new(value).expect("test value must be non-zero")
    }

    fn total_pages<Tag: Clone>(a: &RangeAllocator<Tag>) -> usize {
        let f: usize = a.free_ranges().iter().map(|r| r.pages().get()).sum();
        let l: usize = a.allocated().iter().map(|r| r.pages().get()).sum();
        f + l
    }

    fn arena_pages(start: usize, end: usize) -> usize {
        (end - start) / PAGE_SIZE
    }

    #[test]
    fn preallocates_for_common_small_region_count() {
        let a = allocator();
        assert!(
            a.allocated.capacity() >= 4,
            "default allocator should preallocate a few live-region slots"
        );
        assert!(
            a.allocated.capacity() < DEFAULT_LIVE_REGION_CAPACITY,
            "default allocator should not eagerly reserve the full live-region limit"
        );

        let small: RangeAllocator<u32> = RangeAllocator::with_capacity(
            PageAlignedVirtualAddress::from_usize(REGION_START).unwrap(),
            VirtualAddress::new(REGION_END),
            2,
        );
        assert!(
            small.allocated.capacity() >= 2,
            "allocator should preallocate the requested small live-region limit"
        );
        assert!(
            small.allocated.capacity() <= 2,
            "preallocation must not exceed an explicit small live-region limit"
        );
    }

    #[test]
    fn new_empty_arena_rejects_allocate() {
        let mut a: RangeAllocator<u32> = RangeAllocator::new(
            PageAlignedVirtualAddress::from_usize(REGION_START).unwrap(),
            VirtualAddress::new(REGION_START),
        );
        assert!(a.free_ranges().is_empty());
        assert_eq!(
            a.allocate(nz(PAGE_SIZE), 0),
            Err(AllocateError::NotEnoughSpace)
        );
    }

    #[test]
    fn allocate_takes_from_single_free_range() {
        let mut a = allocator();
        let r = a.allocate(nz(2 * PAGE_SIZE), 7).unwrap();
        assert_eq!(r.base().as_usize(), REGION_START);
        assert_eq!(r.pages().get(), 2);
        assert_eq!(*r.tag(), 7);
        assert_eq!(a.allocated().len(), 1);
        assert_eq!(a.free_ranges().len(), 1);
        assert_eq!(
            a.free_ranges()[0].base().as_usize(),
            REGION_START + 2 * PAGE_SIZE
        );
    }

    #[test]
    fn allocate_then_lookup_finds_region() {
        let mut a = allocator();
        let r = a.allocate(nz(PAGE_SIZE), 42).unwrap();
        let found = a.lookup(r.base(), nz(r.size_bytes())).unwrap();
        assert_eq!(found.base(), r.base());
        assert_eq!(found.pages(), r.pages());
        assert_eq!(*found.tag(), 42);
    }

    #[test]
    fn allocate_unaligned_size_rejected() {
        let mut a = allocator();
        assert_eq!(
            a.allocate(nz(PAGE_SIZE + 1), 0),
            Err(AllocateError::UnalignedSize)
        );
    }

    #[test]
    fn allocate_returns_not_enough_space_when_arena_full() {
        let mut a: RangeAllocator<u32> = RangeAllocator::new(
            PageAlignedVirtualAddress::from_usize(REGION_START).unwrap(),
            VirtualAddress::new(REGION_START + 2 * PAGE_SIZE),
        );
        a.allocate(nz(PAGE_SIZE), 0).unwrap();
        a.allocate(nz(PAGE_SIZE), 0).unwrap();
        assert_eq!(
            a.allocate(nz(PAGE_SIZE), 0),
            Err(AllocateError::NotEnoughSpace)
        );
    }

    #[test]
    fn allocate_returns_out_of_slots_when_capacity_reached() {
        let mut a: RangeAllocator<u32> = RangeAllocator::with_capacity(
            PageAlignedVirtualAddress::from_usize(REGION_START).unwrap(),
            VirtualAddress::new(REGION_END),
            2,
        );
        a.allocate(nz(PAGE_SIZE), 0).unwrap();
        a.allocate(nz(PAGE_SIZE), 0).unwrap();
        assert_eq!(a.allocate(nz(PAGE_SIZE), 0), Err(AllocateError::OutOfSlots));
    }

    #[test]
    fn allocate_first_fit_picks_first_sufficient() {
        // A=1, B=3, C=1, D=2, E=1 - освобождаем B и D, разделённые занятым C;
        // E удерживает D от слияния с "хвостом" арены. Получаем три отдельных
        // свободных диапазона разного размера. Запрос на 2 страницы должен
        // уйти в B (первый достаточно большой), не в D (оптимальный по best-fit).
        let mut a = allocator();
        let _ra = a.allocate(nz(PAGE_SIZE), 0).unwrap();
        let rb = a.allocate(nz(3 * PAGE_SIZE), 0).unwrap();
        let _rc = a.allocate(nz(PAGE_SIZE), 0).unwrap();
        let rd = a.allocate(nz(2 * PAGE_SIZE), 0).unwrap();
        let _re = a.allocate(nz(PAGE_SIZE), 0).unwrap();
        a.free(rb.base(), nz(rb.size_bytes())).unwrap();
        a.free(rd.base(), nz(rd.size_bytes())).unwrap();
        assert_eq!(a.free_ranges().len(), 3);
        assert_eq!(a.free_ranges()[0].pages().get(), 3);
        assert_eq!(a.free_ranges()[1].pages().get(), 2);

        let new = a.allocate(nz(2 * PAGE_SIZE), 9).unwrap();
        assert_eq!(new.base(), rb.base());
    }

    #[test]
    fn allocate_smaller_than_free_range_splits_remainder() {
        let mut a = allocator();
        let r1 = a.allocate(nz(PAGE_SIZE), 0).unwrap();
        let r2 = a.allocate(nz(PAGE_SIZE), 0).unwrap();
        assert_eq!(r2.base().as_usize(), r1.base().as_usize() + PAGE_SIZE);
        assert_eq!(a.free_ranges().len(), 1);
        assert_eq!(
            a.free_ranges()[0].base().as_usize(),
            REGION_START + 2 * PAGE_SIZE
        );
    }

    #[test]
    fn allocate_exact_match_consumes_whole_free_range() {
        let mut a: RangeAllocator<u32> = RangeAllocator::new(
            PageAlignedVirtualAddress::from_usize(REGION_START).unwrap(),
            VirtualAddress::new(REGION_START + 2 * PAGE_SIZE),
        );
        a.allocate(nz(2 * PAGE_SIZE), 0).unwrap();
        assert!(a.free_ranges().is_empty());
    }

    #[test]
    fn free_unknown_base_returns_not_found() {
        let mut a = allocator();
        let bogus = PageAlignedVirtualAddress::from_usize(REGION_START + 16 * PAGE_SIZE).unwrap();
        assert_eq!(a.free(bogus, nz(PAGE_SIZE)), Err(RangeError::NotFound));
    }

    #[test]
    fn free_known_base_wrong_size_returns_not_found() {
        let mut a = allocator();
        let r = a.allocate(nz(2 * PAGE_SIZE), 0).unwrap();
        assert_eq!(a.free(r.base(), nz(PAGE_SIZE)), Err(RangeError::NotFound));
    }

    #[test]
    fn free_unaligned_size_rejected() {
        let mut a = allocator();
        let r = a.allocate(nz(PAGE_SIZE), 0).unwrap();
        assert_eq!(
            a.free(r.base(), nz(PAGE_SIZE + 1)),
            Err(RangeError::UnalignedSize)
        );
    }

    #[test]
    fn free_returns_tag_of_removed_region() {
        let mut a = allocator();
        let r = a.allocate(nz(PAGE_SIZE), 123).unwrap();
        let removed = a.free(r.base(), nz(r.size_bytes())).unwrap();
        assert_eq!(*removed.tag(), 123);
    }

    #[test]
    fn free_then_allocate_reuses_va() {
        let mut a = allocator();
        let r = a.allocate(nz(PAGE_SIZE), 0).unwrap();
        a.free(r.base(), nz(r.size_bytes())).unwrap();
        let again = a.allocate(nz(PAGE_SIZE), 0).unwrap();
        assert_eq!(again.base(), r.base());
    }

    #[test]
    fn free_coalesces_with_left_neighbor() {
        let mut a = allocator();
        let ra = a.allocate(nz(PAGE_SIZE), 0).unwrap();
        let rb = a.allocate(nz(PAGE_SIZE), 0).unwrap();
        // free B первым: получится отдельный free между A и хвостом арены.
        a.free(rb.base(), nz(rb.size_bytes())).unwrap();
        // free A: должен слиться вправо с диапазоном B (а тот в свою очередь
        // с хвостом арены) - итого один свободный диапазон.
        a.free(ra.base(), nz(ra.size_bytes())).unwrap();
        assert_eq!(a.free_ranges().len(), 1);
        assert_eq!(a.free_ranges()[0].base().as_usize(), REGION_START);
    }

    #[test]
    fn free_coalesces_with_right_neighbor() {
        let mut a = allocator();
        let ra = a.allocate(nz(PAGE_SIZE), 0).unwrap();
        let rb = a.allocate(nz(PAGE_SIZE), 0).unwrap();
        // free A первым -> coalesce справа невозможен (B ещё занят), но слева
        // тоже нет соседа: A на самом краю арены. Получаем отдельный free [A].
        a.free(ra.base(), nz(ra.size_bytes())).unwrap();
        // free B: соседство справа (хвост арены) и слева (только что освобождённый A).
        a.free(rb.base(), nz(rb.size_bytes())).unwrap();
        assert_eq!(a.free_ranges().len(), 1);
    }

    #[test]
    fn free_coalesces_both_sides() {
        let mut a = allocator();
        let ra = a.allocate(nz(PAGE_SIZE), 0).unwrap();
        let rb = a.allocate(nz(PAGE_SIZE), 0).unwrap();
        let rc = a.allocate(nz(PAGE_SIZE), 0).unwrap();
        a.free(ra.base(), nz(ra.size_bytes())).unwrap();
        a.free(rc.base(), nz(rc.size_bytes())).unwrap();
        a.free(rb.base(), nz(rb.size_bytes())).unwrap();
        assert_eq!(a.free_ranges().len(), 1);
        assert_eq!(a.free_ranges()[0].base().as_usize(), REGION_START);
        assert_eq!(
            a.free_ranges()[0].pages().get(),
            arena_pages(REGION_START, REGION_END)
        );
    }

    #[test]
    fn free_no_neighbor_inserts_isolated_range() {
        // Сценарий, где освобождаемый диапазон не касается ни одного из
        // существующих свободных. Возможен только при наличии "дыры"
        // занятых регионов вокруг - alloc A, alloc B, alloc C, alloc D, alloc E,
        // free B, free D -> между B и D остаётся занятый C, а далее E и хвост.
        // Освобождение C тогда коалесцится с обеими сторонами; здесь же
        // мы тестируем "изолированное" free B, при котором ни слева, ни справа
        // нет свободных соседей.
        let mut a = allocator();
        let _ra = a.allocate(nz(PAGE_SIZE), 0).unwrap();
        let rb = a.allocate(nz(PAGE_SIZE), 0).unwrap();
        let _rc = a.allocate(nz(PAGE_SIZE), 0).unwrap();
        let free_count_before = a.free_ranges().len();
        a.free(rb.base(), nz(rb.size_bytes())).unwrap();
        assert_eq!(a.free_ranges().len(), free_count_before + 1);
    }

    #[test]
    fn total_pages_invariant_holds() {
        let mut a = allocator();
        let total = arena_pages(REGION_START, REGION_END);
        assert_eq!(total_pages(&a), total);

        let r1 = a.allocate(nz(2 * PAGE_SIZE), 0).unwrap();
        assert_eq!(total_pages(&a), total);
        let r2 = a.allocate(nz(PAGE_SIZE), 0).unwrap();
        assert_eq!(total_pages(&a), total);
        let r3 = a.allocate(nz(4 * PAGE_SIZE), 0).unwrap();
        assert_eq!(total_pages(&a), total);
        a.free(r2.base(), nz(r2.size_bytes())).unwrap();
        assert_eq!(total_pages(&a), total);
        let r4 = a.allocate(nz(PAGE_SIZE), 0).unwrap();
        assert_eq!(total_pages(&a), total);
        a.free(r1.base(), nz(r1.size_bytes())).unwrap();
        assert_eq!(total_pages(&a), total);
        a.free(r3.base(), nz(r3.size_bytes())).unwrap();
        assert_eq!(total_pages(&a), total);
        a.free(r4.base(), nz(r4.size_bytes())).unwrap();
        assert_eq!(total_pages(&a), total);
        assert_eq!(a.free_ranges().len(), 1);
        assert_eq!(a.free_ranges()[0].pages().get(), total);
    }

    #[test]
    fn free_ranges_remain_sorted_after_random_ops() {
        let mut a = allocator();
        let r1 = a.allocate(nz(PAGE_SIZE), 0).unwrap();
        let r2 = a.allocate(nz(PAGE_SIZE), 0).unwrap();
        let r3 = a.allocate(nz(PAGE_SIZE), 0).unwrap();
        let r4 = a.allocate(nz(PAGE_SIZE), 0).unwrap();
        let r5 = a.allocate(nz(PAGE_SIZE), 0).unwrap();
        a.free(r3.base(), nz(r3.size_bytes())).unwrap();
        a.free(r1.base(), nz(r1.size_bytes())).unwrap();
        a.free(r5.base(), nz(r5.size_bytes())).unwrap();
        let _ = (r2, r4);
        for w in a.free_ranges().windows(2) {
            assert!(w[0].base().as_usize() < w[1].base().as_usize());
            assert!(w[0].end_va().as_usize() < w[1].base().as_usize());
        }
    }

    #[test]
    fn allocated_remains_sorted_after_random_ops() {
        let mut a = allocator();
        let r1 = a.allocate(nz(PAGE_SIZE), 0).unwrap();
        let r2 = a.allocate(nz(PAGE_SIZE), 0).unwrap();
        let r3 = a.allocate(nz(PAGE_SIZE), 0).unwrap();
        a.free(r2.base(), nz(r2.size_bytes())).unwrap();
        let r4 = a.allocate(nz(PAGE_SIZE), 0).unwrap();
        let _ = (r1, r3, r4);
        for w in a.allocated().windows(2) {
            assert!(w[0].base().as_usize() < w[1].base().as_usize());
        }
    }

    #[test]
    fn set_tag_updates_existing_region() {
        let mut a = allocator();
        let r = a.allocate(nz(PAGE_SIZE), 1).unwrap();
        let updated = a.set_tag(r.base(), nz(r.size_bytes()), 99).unwrap();
        assert_eq!(*updated.tag(), 99);
        assert_eq!(*a.lookup(r.base(), nz(r.size_bytes())).unwrap().tag(), 99);
    }

    #[test]
    fn set_tag_rejects_partial_match() {
        let mut a = allocator();
        let r = a.allocate(nz(2 * PAGE_SIZE), 0).unwrap();
        assert_eq!(
            a.set_tag(r.base(), nz(PAGE_SIZE), 5),
            Err(RangeError::NotFound)
        );
    }

    #[test]
    fn set_tag_unaligned_size_rejected() {
        let mut a = allocator();
        let r = a.allocate(nz(PAGE_SIZE), 0).unwrap();
        assert_eq!(
            a.set_tag(r.base(), nz(PAGE_SIZE + 1), 5),
            Err(RangeError::UnalignedSize)
        );
    }

    #[test]
    fn lookup_unaligned_size_rejected() {
        let mut a = allocator();
        let r = a.allocate(nz(PAGE_SIZE), 0).unwrap();
        assert_eq!(
            a.lookup(r.base(), nz(PAGE_SIZE + 1)),
            Err(RangeError::UnalignedSize)
        );
    }
}
