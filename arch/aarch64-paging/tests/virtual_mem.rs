mod common;

extern crate alloc;

use aarch64_paging::entry_flags::EntryFlags;
use aarch64_paging::layout::MemoryRegion;
use aarch64_paging::virtual_address::{VirtualAddress, VirtualAddressExt};
use aarch64_paging::virtual_mem::{
    Page, PageTable, PageTableEntry, PageTableManager, VmError, create_page_table_manager,
};
use alloc::vec::Vec;
use common::{
    TEST_FRAME_SIZE, build_frame_allocator, layout_from_regions, make_region,
};
use memory::memory_backend::{MemoryBackend, MockMemoryBackend};
use memory::physical::{Frame, PageAlignedAddress, PhysicalAddress};
use memory::physical_manager::PhysicalMemoryManager;

fn mock_backend(total_frames: usize) -> MockMemoryBackend {
    MockMemoryBackend::new(TEST_FRAME_SIZE, total_frames)
}

fn heap_region(heap_start_frame: usize, heap_frames: usize) -> MemoryRegion<PageAlignedAddress> {
    make_region(
        MemoryRegion::HEAP,
        heap_start_frame,
        heap_frames,
        EntryFlags::KERNEL_DATA,
    )
}

#[test]
fn map_and_translate_single_page() {
    let total_frames = 512;
    let backend = mock_backend(total_frames);
    let allocator = build_frame_allocator(total_frames, &[]);
    let heap = heap_region(32, total_frames - 32);
    let manager =
        PageTableManager::new(allocator, backend.clone(), heap).expect("manager should init");

    let page = Page::containing_address(VirtualAddress::new(0x4000_0000));
    let frame = Frame::new(200);
    manager
        .map(page, frame, EntryFlags::KERNEL_DATA)
        .expect("map succeeds");

    let translated = manager
        .translate(page.start_address())
        .expect("translation succeeds");
    assert_eq!(translated.as_usize(), frame.page_address().as_usize());

    let _ = verify_leaf_entry(&backend, &manager, page, frame);
}

#[test]
fn map_range_maps_multiple_pages() {
    let total_frames = 512;
    let backend = mock_backend(total_frames);
    let allocator = build_frame_allocator(total_frames, &[]);
    let heap = heap_region(64, total_frames - 64);
    let manager =
        PageTableManager::new(allocator, backend.clone(), heap).expect("manager should init");

    let start_page = Page::containing_address(VirtualAddress::new(0x8000_0000));
    let end_page = start_page.next().next();
    let pages: Vec<_> = Page::range_inclusive(start_page, end_page).collect();
    let frames: Vec<_> = (300..=302).map(Frame::new).collect();

    manager
        .map_range(
            start_page,
            Frame::new(300),
            pages.len(),
            EntryFlags::KERNEL_DATA,
        )
        .expect("map_range ok");

    for (page, frame) in pages.iter().zip(frames.iter()) {
        let translated = manager
            .translate(page.start_address())
            .expect("translation succeeds");
        assert_eq!(translated.as_usize(), frame.page_address().as_usize());
    }
}

#[test]
fn map_existing_page_fails() {
    let total_frames = 256;
    let backend = mock_backend(total_frames);
    let allocator = build_frame_allocator(total_frames, &[]);
    let heap = heap_region(32, total_frames - 32);
    let manager =
        PageTableManager::new(allocator, backend.clone(), heap).expect("manager should init");

    let page = Page::containing_address(VirtualAddress::new(0x2000_0000));
    let frame = Frame::new(120);

    manager
        .map(page, frame, EntryFlags::KERNEL_DATA)
        .expect("first map succeeds");

    let err = manager
        .map(page, Frame::new(121), EntryFlags::KERNEL_DATA)
        .unwrap_err();
    assert!(matches!(err, VmError::AlreadyMapped));
}

#[test]
fn unmap_returns_original_frame_and_clears_entry() {
    let total_frames = 512;
    let backend = mock_backend(total_frames);
    let allocator = build_frame_allocator(total_frames, &[]);
    let heap = heap_region(32, total_frames - 32);
    let manager =
        PageTableManager::new(allocator, backend.clone(), heap).expect("manager should init");

    let page = Page::containing_address(VirtualAddress::new(0x6000_0000));
    let frame = Frame::new(180);
    let leaf_frame = verify_leaf_entry_after_map(&backend, &manager, page, frame);

    let unmapped = manager.unmap(page).expect("unmap succeeds");
    assert_eq!(unmapped.number(), frame.number());
    assert!(
        manager.translate(page.start_address()).is_none(),
        "translation should fail after unmap"
    );

    let leaf_addr = leaf_frame.page_address();
    let leaf_table: PageTable = backend.read(leaf_addr.as_physical_address());
    let idx = page.start_address().page_table_indices()[3];
    assert!(
        !leaf_table[idx].is_valid(),
        "leaf entry must be cleared after unmap"
    );
}

#[test]
fn unmap_missing_page_returns_error() {
    let total_frames = 256;
    let backend = mock_backend(total_frames);
    let allocator = build_frame_allocator(total_frames, &[]);
    let heap = heap_region(32, total_frames - 32);
    let manager =
        PageTableManager::new(allocator, backend.clone(), heap).expect("manager should init");

    let page = Page::containing_address(VirtualAddress::new(0x6100_0000));
    let err = manager.unmap(page).unwrap_err();
    assert!(matches!(err, VmError::NotMapped));
}

#[test]
fn block_mapping_conflict_is_reported() {
    let total_frames = 256;
    let backend = mock_backend(total_frames);
    let allocator = build_frame_allocator(total_frames, &[]);
    let heap = heap_region(64, total_frames - 64);
    let manager =
        PageTableManager::new(allocator, backend.clone(), heap).expect("manager should init");

    let page = Page::containing_address(VirtualAddress::new(0x9000_0000));
    let indices = page.start_address().page_table_indices();
    let root_addr = manager.root_frame().page_address();
    let mut root: PageTable = backend.read(root_addr.as_physical_address());
    // Создаем block descriptor (VALID=1, TABLE/PAGE=0)
    let block_flags = EntryFlags::KERNEL_DATA
        .set(EntryFlags::VALID)
        .set(EntryFlags::ACCESS);
    root[indices[0]].set(PageTableEntry::new_frame(Frame::new(10), block_flags));
    backend.write(root_addr.as_physical_address(), root);

    let err = manager
        .map(page, Frame::new(200), EntryFlags::KERNEL_DATA)
        .unwrap_err();
    assert!(matches!(err, VmError::BlockMappingExists));
}

#[test]
fn identity_regions_are_direct_mapped() {
    let total_frames = 512;
    let backend = mock_backend(total_frames);
    let allocator = build_frame_allocator(total_frames, &[]);
    let mut kernel_region = make_region("kernel", 4, 4, EntryFlags::KERNEL_CODE);
    kernel_region.identity_map = true;
    let mut dtb_region = make_region("dtb", 8, 2, EntryFlags::DEVICE);
    dtb_region.identity_map = true;
    let heap_region = make_region(MemoryRegion::HEAP, 64, total_frames - 64, EntryFlags::KERNEL_DATA);
    let layout = layout_from_regions(kernel_region, dtb_region, heap_region);
    let identity = [kernel_region, dtb_region];

    let manager =
        create_page_table_manager(allocator, backend.clone(), layout).expect("manager init");

    for region in identity {
        for addr in (region.start.as_usize()..=region.end.as_usize()).step_by(TEST_FRAME_SIZE) {
            let virt = VirtualAddress::new(addr);
            let phys = manager.translate(virt).expect("address must be mapped");
            assert_eq!(
                phys.as_usize(),
                addr,
                "identity map should keep address for region {}",
                region.label
            );
        }
    }
}

#[test]
fn enable_virtual_mode_records_root_page() {
    let total_frames = 256;
    let backend = mock_backend(total_frames);
    let allocator = build_frame_allocator(total_frames, &[]);
    let heap = heap_region(32, total_frames - 32);
    let manager =
        PageTableManager::new(allocator, backend.clone(), heap).expect("manager should init");

    manager.enable_virtual_mode();

    let recorded = backend
        .last_root_page()
        .expect("backend should store root page")
        .as_usize();
    let expected = manager.root_frame().page_address().as_usize();
    assert_eq!(recorded, expected);
}

#[test]
fn adjacent_identity_regions_with_misaligned_boundaries_fail() {
    let total_frames = 512;
    let backend = mock_backend(total_frames);
    let allocator = build_frame_allocator(total_frames, &[]);

    // Создаем два смежных региона с НЕВЫРОВНЕННЫМИ границами и РАЗНЫМИ флагами
    // Оба региона пытаются замапить фрейм 5 с разными флагами - должна быть ошибка
    let frame_size = TEST_FRAME_SIZE;

    // kernel_code: заканчивается в середине фрейма 5
    let kernel_start = common::frame_to_address(4);
    let kernel_end_unaligned = frame_size * 5 + frame_size / 2;
    let kernel_end = PageAlignedAddress::aligned_up(PhysicalAddress::from(kernel_end_unaligned));
    let kernel_region = MemoryRegion {
        label: "kernel_code",
        start: kernel_start,
        end: kernel_end,
        flags: EntryFlags::KERNEL_CODE,
        identity_map: true,
    };

    // kernel_rodata: начинается в середине фрейма 5 (делит фрейм с kernel_code!)
    let rodata_start = PageAlignedAddress::new_unchecked(PhysicalAddress::from(
        frame_size * 5 + frame_size / 2 + 1,
    ));
    let rodata_end_unaligned = frame_size * 8 - 1;
    // Округляем вверх до границы следующего фрейма для exclusive границы
    let rodata_end = PageAlignedAddress::aligned_up(PhysicalAddress::from(rodata_end_unaligned));
    let rodata_region = MemoryRegion {
        label: "kernel_rodata",
        start: rodata_start,
        end: rodata_end,
        flags: EntryFlags::KERNEL_RODATA,
        identity_map: true,
    };

    let heap_region = make_region(MemoryRegion::HEAP, 64, total_frames - 64, EntryFlags::KERNEL_DATA);
    let layout = layout_from_regions(kernel_region, rodata_region, heap_region);

    // Попытка замапить регионы с конфликтующими флагами должна вызвать ошибку AlreadyMapped
    let result = create_page_table_manager(allocator, backend.clone(), layout);
    match result {
        Err(VmError::AlreadyMapped) => {
            // Ожидаемая ошибка - тест пройден
        }
        Err(e) => panic!("Expected AlreadyMapped, got {:?}", e),
        Ok(_) => panic!("Should fail when regions with different flags share a frame"),
    }
}

#[test]
fn adjacent_identity_regions_with_aligned_boundaries_succeed() {
    let total_frames = 512;
    let backend = mock_backend(total_frames);
    let allocator = build_frame_allocator(total_frames, &[]);

    // Создаем два смежных региона с ВЫРОВНЕННЫМИ границами - не должно быть конфликтов
    let mut kernel_region = make_region("kernel", 4, 3, EntryFlags::KERNEL_CODE);
    kernel_region.identity_map = true;
    let mut rodata_region = make_region("rodata", 7, 2, EntryFlags::KERNEL_RODATA);
    rodata_region.identity_map = true;
    let heap_region = make_region(MemoryRegion::HEAP, 64, total_frames - 64, EntryFlags::KERNEL_DATA);
    let layout = layout_from_regions(kernel_region, rodata_region, heap_region);

    let manager = create_page_table_manager(allocator, backend.clone(), layout)
        .expect("Should succeed when region boundaries are frame-aligned");

    // Проверяем, что оба региона корректно замаплены
    // region.end - это exclusive граница, поэтому используем ..region.end
    let identity = [kernel_region, rodata_region];
    for region in identity {
        for addr in (region.start.as_usize()..region.end.as_usize()).step_by(TEST_FRAME_SIZE) {
            let virt = VirtualAddress::new(addr);
            let phys = manager.translate(virt).expect("address must be mapped");
            assert_eq!(
                phys.as_usize(),
                addr,
                "identity map for region {}",
                region.label
            );
        }
    }
}

fn verify_leaf_entry_after_map(
    backend: &MockMemoryBackend,
    manager: &PageTableManager<PhysicalMemoryManager, MockMemoryBackend>,
    page: Page,
    frame: Frame,
) -> Frame {
    manager
        .map(page, frame, EntryFlags::KERNEL_DATA)
        .expect("map succeeds");
    verify_leaf_entry(backend, manager, page, frame)
}

fn verify_leaf_entry(
    backend: &MockMemoryBackend,
    manager: &PageTableManager<PhysicalMemoryManager, MockMemoryBackend>,
    page: Page,
    frame: Frame,
) -> Frame {
    let indices = page.start_address().page_table_indices();
    let mut table_frame = manager.root_frame();

    for level in 0..3 {
        let table_addr = table_frame.page_address();
        let table: PageTable = backend.read(table_addr.as_physical_address());
        let entry = table[indices[level]];
        assert!(entry.is_valid(), "level {level} entry must be valid");
        if level < 2 {
            assert!(entry.is_table(), "level {level} must point to table");
            table_frame = entry.frame();
        } else {
            assert!(
                entry.is_table(),
                "level 2 entry must be a table to reach leaf level"
            );
            table_frame = entry.frame();
        }
    }

    let leaf_addr = table_frame.page_address();
    let leaf: PageTable = backend.read(leaf_addr.as_physical_address());
    let leaf_entry = leaf[indices[3]];
    assert!(leaf_entry.is_valid(), "leaf entry must be valid");
    assert_eq!(
        leaf_entry.address().as_usize(),
        frame.page_address().as_usize()
    );

    table_frame
}
