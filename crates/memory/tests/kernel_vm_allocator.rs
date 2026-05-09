//! Unit-тесты `HeapAllocator`. Mock-mapper использует backing-буфер
//! напрямую как арену heap'а - адрес буфера становится `arena_base`,
//! поэтому VA-операции реально пишут в host-память.

#![allow(unsafe_code)]

mod common;

use core::{alloc::Layout, cell::Cell, ptr::NonNull};

use memory::{
    MemFlags,
    kernel_vm_allocator::{AllocationError, HeapAllocator, HeapArena},
    memory_mapper::{
        AddressSpaceHandle, AddressSpaceTag, MemoryMapper, MemoryMappingError,
        MemoryRemappingError, MemoryUnmappingError,
    },
    physical_address::{PageAlignedAddress, PhysicalAddress},
    virtual_address::PageAlignedVirtualAddress,
};

const PAGE_SIZE: usize = 4096;

struct MockKernelMapper {
    arena_base: usize,
    arena_size: usize,
    /// Сколько успешных map-вызовов осталось до возврата OOM (None = не падать).
    map_calls_until_fail: Cell<Option<usize>>,
    pages_mapped: Cell<usize>,
    /// Сколько байт арены "поднято" - для проверки, что heap не выходит за
    /// arena_top и зовёт map строго в следующий слот.
    arena_top: Cell<usize>,
}

// SAFETY: тесты однопоточные.
unsafe impl Send for MockKernelMapper {}
// SAFETY: см. Send.
unsafe impl Sync for MockKernelMapper {}

impl MockKernelMapper {
    fn new(arena_base: usize, arena_size: usize) -> Self {
        Self {
            arena_base,
            arena_size,
            map_calls_until_fail: Cell::new(None),
            pages_mapped: Cell::new(0),
            arena_top: Cell::new(0),
        }
    }

    fn fail_after(&self, n: usize) {
        self.map_calls_until_fail.set(Some(n));
    }

    fn pages_mapped(&self) -> usize {
        self.pages_mapped.get()
    }
}

impl MemoryMapper for MockKernelMapper {
    fn map(
        &self,
        va: PageAlignedVirtualAddress,
        page_count: usize,
        init: &[u8],
        _flags: MemFlags,
    ) -> Result<(), MemoryMappingError> {
        let bytes = page_count * PAGE_SIZE;
        let va_usize = va.as_usize();

        assert_eq!(
            va_usize,
            self.arena_base + self.arena_top.get(),
            "heap.expand must map at the next arena top"
        );
        assert!(
            self.arena_top.get() + bytes <= self.arena_size,
            "heap.expand exceeded arena size"
        );

        if let Some(left) = self.map_calls_until_fail.get() {
            if left == 0 {
                return Err(MemoryMappingError::OutOfMemory);
            }
            self.map_calls_until_fail.set(Some(left - 1));
        }

        // SAFETY: backing-буфер занимает диапазон
        // [arena_base, arena_base + arena_size); запись внутри корректна.
        unsafe {
            core::ptr::write_bytes(va_usize as *mut u8, 0, bytes);
            if !init.is_empty() {
                let take = init.len().min(bytes);
                core::ptr::copy_nonoverlapping(init.as_ptr(), va_usize as *mut u8, take);
            }
        }

        self.arena_top.set(self.arena_top.get() + bytes);
        self.pages_mapped.set(self.pages_mapped.get() + page_count);
        Ok(())
    }

    fn map_exact(
        &self,
        _source: PageAlignedVirtualAddress,
        _target: PageAlignedAddress,
        _size: usize,
        _flags: MemFlags,
    ) -> Result<(), MemoryMappingError> {
        unimplemented!("not used by HeapAllocator unit tests")
    }

    fn unmap(
        &self,
        _address: PageAlignedVirtualAddress,
        _size: usize,
    ) -> Result<(), MemoryUnmappingError> {
        unimplemented!("not used by HeapAllocator unit tests")
    }

    fn remap(
        &self,
        _start: PageAlignedVirtualAddress,
        _size: usize,
        _new_flags: MemFlags,
    ) -> Result<(), MemoryRemappingError> {
        unimplemented!("not used by HeapAllocator unit tests")
    }

    fn activate_handle(&self) -> AddressSpaceHandle {
        AddressSpaceHandle::new(PhysicalAddress::new(0), AddressSpaceTag::NONE)
    }

    fn as_any(&self) -> &(dyn core::any::Any + 'static) {
        self
    }
}

const DEFAULT_ARENA_SIZE: usize = 256 * 1024;

fn make_heap(arena_size: usize) -> (HeapAllocator, &'static MockKernelMapper) {
    let buffer = vec![0u8; arena_size + PAGE_SIZE].into_boxed_slice();
    let raw = Box::leak(buffer);
    let raw_addr = raw.as_ptr() as usize;
    let aligned_base = (raw_addr + PAGE_SIZE - 1) & !(PAGE_SIZE - 1);

    let mapper = Box::leak(Box::new(MockKernelMapper::new(aligned_base, arena_size)));
    let mapper_dyn: &'static (dyn MemoryMapper + Send + Sync) = mapper;

    let arena_base = PageAlignedVirtualAddress::from_usize(aligned_base)
        .expect("page-aligned arena base from align math");
    let heap = HeapAllocator::new(mapper_dyn, HeapArena::new(arena_base, arena_size));
    (heap, mapper)
}

fn make_default_heap() -> (HeapAllocator, &'static MockKernelMapper) {
    make_heap(DEFAULT_ARENA_SIZE)
}

// =============================================================================
// 1. Базовое выделение
// =============================================================================

#[test]
fn allocate_returns_valid_pointer() {
    let (mut heap, _m) = make_default_heap();
    let layout = Layout::from_size_align(64, 8).unwrap();
    assert!(heap.allocate(layout).is_ok());
}

#[test]
fn allocate_returns_aligned_pointer() {
    let (mut heap, _m) = make_default_heap();
    for align in [8usize, 16, 32, 64, 128] {
        let layout = Layout::from_size_align(32, align).unwrap();
        let ptr = heap.allocate(layout).unwrap();
        assert_eq!(
            ptr.as_ptr() as usize % align,
            0,
            "pointer should be aligned to {align}"
        );
    }
}

#[test]
fn multiple_allocations_return_distinct_pointers() {
    let (mut heap, _m) = make_default_heap();
    let layout = Layout::from_size_align(64, 8).unwrap();
    let p1 = heap.allocate(layout).unwrap();
    let p2 = heap.allocate(layout).unwrap();
    let p3 = heap.allocate(layout).unwrap();
    assert_ne!(p1.as_ptr(), p2.as_ptr());
    assert_ne!(p2.as_ptr(), p3.as_ptr());
    assert_ne!(p1.as_ptr(), p3.as_ptr());
}

#[test]
fn allocated_memory_is_writable() {
    let (mut heap, _m) = make_default_heap();
    let layout = Layout::from_size_align(128, 8).unwrap();
    let ptr = heap.allocate(layout).unwrap();
    // SAFETY: ptr - свежевыделенный 128-байтный блок, эксклюзивно наш.
    unsafe {
        let slice = core::slice::from_raw_parts_mut(ptr.as_ptr(), 128);
        for (i, b) in slice.iter_mut().enumerate() {
            *b = (i % 256) as u8;
        }
        for (i, b) in slice.iter().enumerate() {
            assert_eq!(*b, (i % 256) as u8);
        }
    }
}

// =============================================================================
// 2. Главное: aligned-аллокация (изначальный bug)
// =============================================================================

#[test]
fn aligned_4k_4k_allocation_succeeds_on_fresh_heap() {
    // Изначальный bug: до vmalloc-style refactor'а этот вызов мог
    // вернуть null, если physical-bitmap имел изолированную дырку.
    let (mut heap, mapper) = make_heap(8 * PAGE_SIZE);
    let layout = Layout::from_size_align(PAGE_SIZE, PAGE_SIZE).unwrap();
    let ptr = heap.allocate(layout).expect("4K-aligned 4K alloc");
    assert_eq!(ptr.as_ptr() as usize % PAGE_SIZE, 0);
    assert!(
        mapper.pages_mapped() >= 2,
        "expand must have mapped >= 2 pages"
    );
}

#[test]
fn aligned_2k_alignment_succeeds() {
    let (mut heap, _m) = make_default_heap();
    let layout = Layout::from_size_align(64, 2048).unwrap();
    let ptr = heap.allocate(layout).expect("2K-aligned alloc");
    assert_eq!(ptr.as_ptr() as usize % 2048, 0);
}

#[test]
fn aligned_8k_alignment_succeeds() {
    let (mut heap, _m) = make_default_heap();
    let layout = Layout::from_size_align(4096, 8192).unwrap();
    let ptr = heap.allocate(layout).expect("8K-aligned alloc");
    assert_eq!(ptr.as_ptr() as usize % 8192, 0);
}

#[test]
fn many_aligned_4k_allocations_succeed() {
    let (mut heap, _m) = make_heap(64 * PAGE_SIZE);
    let layout = Layout::from_size_align(PAGE_SIZE, PAGE_SIZE).unwrap();
    let mut ptrs = Vec::new();
    for _ in 0..10 {
        let p = heap.allocate(layout).expect("aligned 4K alloc in series");
        assert_eq!(p.as_ptr() as usize % PAGE_SIZE, 0);
        ptrs.push(p);
    }
    // Все указатели уникальны.
    for i in 0..ptrs.len() {
        for j in (i + 1)..ptrs.len() {
            assert_ne!(ptrs[i].as_ptr(), ptrs[j].as_ptr());
        }
    }
}

// =============================================================================
// 3. Обработка ошибок
// =============================================================================

#[test]
fn zero_size_allocation_returns_invalid_layout() {
    let (mut heap, _m) = make_default_heap();
    let layout = Layout::from_size_align(0, 1).unwrap();
    let r = heap.allocate(layout);
    assert!(matches!(r, Err(AllocationError::InvalidLayout)));
}

#[test]
fn arena_exhaustion_returns_out_of_memory() {
    // 4 страницы арены - можно выделить около 3-4 раз 8K-блоки и упереться.
    let (mut heap, _m) = make_heap(4 * PAGE_SIZE);
    let layout = Layout::from_size_align(8192, 8).unwrap();
    let mut allocations = 0usize;
    loop {
        match heap.allocate(layout) {
            Ok(_) => allocations += 1,
            Err(AllocationError::OutOfMemory) => break,
            Err(other) => panic!("unexpected error: {other:?}"),
        }
        assert!(allocations <= 100, "heap should have been exhausted");
    }
    assert!(allocations >= 1, "at least one alloc must have succeeded");
}

#[test]
fn mapper_failure_propagates_as_oom() {
    // Мок-mapper падает на первом же expand-вызове.
    let (mut heap, mapper) = make_default_heap();
    mapper.fail_after(0);
    let layout = Layout::from_size_align(64, 8).unwrap();
    let r = heap.allocate(layout);
    assert!(matches!(r, Err(AllocationError::OutOfMemory)));
}

// =============================================================================
// 4. Освобождение и повторное использование
// =============================================================================

#[test]
fn deallocate_allows_reuse() {
    let (mut heap, _m) = make_default_heap();
    let layout = Layout::from_size_align(1024, 8).unwrap();
    let p1 = heap.allocate(layout).unwrap();
    heap.deallocate(p1);
    let _p2 = heap.allocate(layout).unwrap();
}

#[test]
fn deallocate_three_blocks_in_reverse_order() {
    let (mut heap, _m) = make_default_heap();
    let layout = Layout::from_size_align(512, 8).unwrap();
    let p1 = heap.allocate(layout).unwrap();
    let p2 = heap.allocate(layout).unwrap();
    let p3 = heap.allocate(layout).unwrap();

    heap.deallocate(p3);
    heap.deallocate(p2);
    heap.deallocate(p1);

    let n1 = heap.allocate(layout).unwrap();
    let n2 = heap.allocate(layout).unwrap();
    let n3 = heap.allocate(layout).unwrap();
    assert_ne!(n1.as_ptr(), n2.as_ptr());
    assert_ne!(n2.as_ptr(), n3.as_ptr());
    assert_ne!(n1.as_ptr(), n3.as_ptr());
}

#[test]
fn deallocate_middle_block_reuses_slot() {
    let (mut heap, _m) = make_default_heap();
    let layout = Layout::from_size_align(256, 8).unwrap();
    let p1 = heap.allocate(layout).unwrap();
    let p2 = heap.allocate(layout).unwrap();
    let p3 = heap.allocate(layout).unwrap();
    heap.deallocate(p2);
    let new_p = heap.allocate(layout).unwrap();
    assert_ne!(new_p.as_ptr(), p1.as_ptr());
    assert_ne!(new_p.as_ptr(), p3.as_ptr());
}

#[test]
fn coalescing_allows_large_allocation_after_fragmentation() {
    // Маленькие соседние свободные блоки должны слиться при возврате,
    // позволяя повторное выделение блока, превышающего любой
    // отдельный фрагмент.
    let (mut heap, _m) = make_heap(2 * PAGE_SIZE);
    let small = Layout::from_size_align(1000, 8).unwrap();
    let p1 = heap.allocate(small).unwrap();
    let p2 = heap.allocate(small).unwrap();
    let p3 = heap.allocate(small).unwrap();

    heap.deallocate(p1);
    heap.deallocate(p3);
    heap.deallocate(p2);

    let big = Layout::from_size_align(3500, 8).unwrap();
    assert!(heap.allocate(big).is_ok(), "coalesced free block must fit");
}

#[test]
fn double_free_is_safely_ignored() {
    let (mut heap, _m) = make_default_heap();
    let layout = Layout::from_size_align(64, 8).unwrap();
    let p = heap.allocate(layout).unwrap();
    heap.deallocate(p);
    heap.deallocate(p); // должен молча проигнорироваться

    let n1 = heap.allocate(layout).unwrap();
    let n2 = heap.allocate(layout).unwrap();
    assert_ne!(n1.as_ptr(), n2.as_ptr());
}

#[test]
fn deallocate_pointer_below_arena_is_ignored() {
    let (mut heap, _m) = make_default_heap();
    // Выполним один alloc для подъёма арены, затем dealloc указателя ниже.
    let _ = heap
        .allocate(Layout::from_size_align(64, 8).unwrap())
        .unwrap();
    let bogus = NonNull::new(0x1usize as *mut u8).unwrap();
    heap.deallocate(bogus); // не должен паниковать
}

#[test]
fn deallocate_pointer_above_arena_is_ignored() {
    let (mut heap, _m) = make_default_heap();
    let _ = heap
        .allocate(Layout::from_size_align(64, 8).unwrap())
        .unwrap();
    // Указатель далеко за пределы арены.
    let bogus = NonNull::new((!0usize / 2) as *mut u8).unwrap();
    heap.deallocate(bogus);
}

// =============================================================================
// 5. Расширение арены
// =============================================================================

#[test]
fn allocator_expands_on_demand() {
    let (mut heap, mapper) = make_default_heap();
    let layout = Layout::from_size_align(8192, 8).unwrap();
    assert!(heap.allocate(layout).is_ok());
    assert!(heap.allocate(layout).is_ok());
    assert!(heap.allocate(layout).is_ok());
    assert!(mapper.pages_mapped() > 0);
}

#[test]
fn varying_sizes_work() {
    let (mut heap, _m) = make_default_heap();
    for &size in &[16, 64, 128, 32, 256, 48, 512, 1024, 2048] {
        let layout = Layout::from_size_align(size, 8).unwrap();
        assert!(heap.allocate(layout).is_ok(), "alloc {size}B failed");
    }
}

// =============================================================================
// 6. Свойства реальной integration-нагрузки
// =============================================================================

#[test]
fn many_small_allocations_with_writes() {
    let (mut heap, _m) = make_default_heap();
    let layout = Layout::from_size_align(16, 8).unwrap();
    let mut ptrs = Vec::new();
    for i in 0u8..100 {
        match heap.allocate(layout) {
            Ok(p) => {
                // SAFETY: p - свежевыделенный 16-байтный блок.
                unsafe { core::ptr::write_bytes(p.as_ptr(), i, 16) };
                ptrs.push((p, i));
            }
            Err(AllocationError::OutOfMemory) => break,
            Err(e) => panic!("unexpected: {e:?}"),
        }
    }
    assert!(ptrs.len() >= 50, "expected lots of small allocs");
    for (p, pat) in &ptrs {
        // SAFETY: блоки не освобождались, slice валиден.
        unsafe {
            let slice = core::slice::from_raw_parts(p.as_ptr(), 16);
            assert!(slice.iter().all(|&b| b == *pat), "block {pat} corrupted");
        }
    }
}

#[test]
fn allocations_do_not_overlap() {
    let (mut heap, _m) = make_default_heap();
    let layout = Layout::from_size_align(128, 8).unwrap();
    let p1 = heap.allocate(layout).unwrap();
    let p2 = heap.allocate(layout).unwrap();
    let p3 = heap.allocate(layout).unwrap();
    // SAFETY: 3 непересекающихся выделенных блока.
    unsafe {
        core::ptr::write_bytes(p1.as_ptr(), 0xAA, 128);
        core::ptr::write_bytes(p2.as_ptr(), 0xBB, 128);
        core::ptr::write_bytes(p3.as_ptr(), 0xCC, 128);
        let s1 = core::slice::from_raw_parts(p1.as_ptr(), 128);
        let s2 = core::slice::from_raw_parts(p2.as_ptr(), 128);
        let s3 = core::slice::from_raw_parts(p3.as_ptr(), 128);
        assert!(s1.iter().all(|&b| b == 0xAA));
        assert!(s2.iter().all(|&b| b == 0xBB));
        assert!(s3.iter().all(|&b| b == 0xCC));
    }
}

#[test]
fn block_too_small_to_split_uses_whole_block() {
    let (mut heap, _m) = make_default_heap();
    let big = Layout::from_size_align(3800, 8).unwrap();
    let p = heap.allocate(big).unwrap();
    heap.deallocate(p);

    let almost = Layout::from_size_align(3780, 8).unwrap();
    assert!(heap.allocate(almost).is_ok());
}

#[test]
fn current_size_grows_with_expands() {
    let (mut heap, _m) = make_default_heap();
    let initial = heap.current_size();
    let _ = heap
        .allocate(Layout::from_size_align(128, 8).unwrap())
        .unwrap();
    let after_first = heap.current_size();
    assert!(after_first > initial, "expand must increase current_size");
    // Большое выделение, требующее повторного expand'а.
    let _ = heap
        .allocate(Layout::from_size_align(10 * PAGE_SIZE, 8).unwrap())
        .unwrap();
    let after_big = heap.current_size();
    assert!(after_big > after_first);
}

#[test]
fn stress_allocate_deallocate_loop() {
    let (mut heap, _m) = make_heap(64 * PAGE_SIZE);
    let layout = Layout::from_size_align(256, 8).unwrap();
    let mut alive: Vec<NonNull<u8>> = Vec::new();

    for round in 0usize..1000 {
        if round.is_multiple_of(3) && !alive.is_empty() {
            let idx = round % alive.len();
            heap.deallocate(alive.remove(idx));
        } else {
            match heap.allocate(layout) {
                Ok(p) => alive.push(p),
                Err(AllocationError::OutOfMemory) => {
                    while alive.len() > alive.capacity() / 2 && !alive.is_empty() {
                        heap.deallocate(alive.pop().unwrap());
                    }
                }
                Err(e) => panic!("unexpected: {e:?}"),
            }
        }
    }
}
