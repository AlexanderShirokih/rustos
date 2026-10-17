#![allow(clippy::unwrap_used)]

mod common;

use common::{MockAlloc, allocate_buffer, make_mapper};
use hal_aarch64_paging::{
    level::{L1, L2, L3, Level},
    linear_map::{LINEAR_RANGES_MAX, LinearMap, LinearMapError},
    mapper::PageMapper,
    mem_flags::{Aarch64MemFlags, Access, Shareability},
    memory_attribute::MemoryAttribute,
    preset::{KernelData, KernelRoData, Mmio},
    user_copy::{UserAccess, kernel_alias},
};
use memory::{
    memory_mapper::UserCopyError,
    physical_address::{AlignedPhysicalAddress, PhysicalAddress},
    virtual_address::{AlignedVirtualAddress, PageAlignedVirtualAddress, VirtualAddress},
};

const LINEAR_OFFSET: usize = 0xFFFF_FF80_0000_0000;
const LINEAR_WINDOW: usize = 0x78_0000_0000;

const USER_PAGE: usize = 0x4000_1000;
const RAM_BLOCK: usize = 0x4020_0000;
const RAM_FRAME: usize = RAM_BLOCK + 0x3000;
const UNMAPPED_FRAME: usize = 0x0900_0000;
const PAST_LINEAR_WINDOW: usize = 0x80_0000_0000;
const BLOCK_SIZE: usize = 1 << L2::SHIFT;

fn user_flags(ap: Access, attribute: MemoryAttribute) -> Aarch64MemFlags {
    Aarch64MemFlags::new()
        .af(true)
        .sh(Shareability::Inner)
        .ap(ap)
        .attr_index(attribute.index())
        .pxn(true)
        .uxn(true)
}

fn user_space(pa: usize, flags: Aarch64MemFlags) -> PageMapper<MockAlloc> {
    let (mut user, _) = make_mapper(allocate_buffer(8));
    user.map_page(
        AlignedVirtualAddress::<{ L3::SHIFT }>::from_usize(USER_PAGE).unwrap(),
        AlignedPhysicalAddress::<{ L3::SHIFT }>::from_usize(pa).unwrap(),
        flags,
    )
    .unwrap();
    user
}

fn kernel_space(blocks: &[(usize, usize, Aarch64MemFlags)]) -> PageMapper<MockAlloc> {
    let (mut kernel, _) = make_mapper(allocate_buffer(16));
    for &(alias, block, flags) in blocks {
        kernel
            .map_page(
                AlignedVirtualAddress::<{ L2::SHIFT }>::from_usize(alias).unwrap(),
                AlignedPhysicalAddress::<{ L2::SHIFT }>::from_usize(block).unwrap(),
                flags,
            )
            .unwrap();
    }
    kernel
}

fn capture(kernel: &PageMapper<MockAlloc>) -> Result<LinearMap, LinearMapError> {
    LinearMap::capture(
        kernel,
        PageAlignedVirtualAddress::from_usize(LINEAR_OFFSET).unwrap(),
        LINEAR_WINDOW,
    )
}

fn ram_linear_map() -> LinearMap {
    capture(&kernel_space(&[(
        LINEAR_OFFSET + RAM_BLOCK,
        RAM_BLOCK,
        KernelData::flags(),
    )]))
    .unwrap()
}

fn alias(
    user: &PageMapper<MockAlloc>,
    linear: &LinearMap,
    offset: usize,
    access: UserAccess,
) -> Result<VirtualAddress, UserCopyError> {
    kernel_alias(
        user,
        linear,
        VirtualAddress::new(USER_PAGE + offset),
        access,
    )
}

fn linear_alias(linear: &LinearMap, pa: usize, access: UserAccess) -> Option<usize> {
    linear
        .alias(PhysicalAddress::new(pa), access)
        .map(VirtualAddress::as_usize)
}

#[test]
fn a_cached_user_page_resolves_to_its_frame_in_the_linear_map() {
    let user = user_space(
        RAM_FRAME,
        user_flags(Access::UserRW, MemoryAttribute::Cached),
    );
    let linear = ram_linear_map();

    assert_eq!(
        alias(&user, &linear, 0x10, UserAccess::Write),
        Ok(VirtualAddress::new(LINEAR_OFFSET + RAM_FRAME + 0x10))
    );
    assert_eq!(
        alias(&user, &linear, 0x1000, UserAccess::Read),
        Err(UserCopyError::NotMapped)
    );
}

// Копирование через кэшируемую карту нарушает атрибуты Device/WC, а обращение к фрейму
// вне карты даёт translation fault на EL1.
#[test]
fn user_copy_refuses_uncached_pages_and_frames_outside_the_linear_map() {
    let linear = ram_linear_map();

    for (frame, attribute) in [
        (RAM_FRAME, MemoryAttribute::Device),
        (RAM_FRAME, MemoryAttribute::WriteCombining),
        (UNMAPPED_FRAME, MemoryAttribute::Cached),
        (PAST_LINEAR_WINDOW, MemoryAttribute::Cached),
    ] {
        let user = user_space(frame, user_flags(Access::UserRW, attribute));
        assert_eq!(
            alias(&user, &linear, 0, UserAccess::Read),
            Err(UserCopyError::AccessDenied)
        );
    }
}

#[test]
fn user_permissions_gate_each_access() {
    let linear = ram_linear_map();
    let readable = Ok(VirtualAddress::new(LINEAR_OFFSET + RAM_FRAME));
    let denied = Err(UserCopyError::AccessDenied);
    for (ap, read) in [(Access::UserRO, readable), (Access::KernelRW, denied)] {
        let user = user_space(RAM_FRAME, user_flags(ap, MemoryAttribute::Cached));
        assert_eq!(alias(&user, &linear, 0, UserAccess::Read), read);
        assert_eq!(alias(&user, &linear, 0, UserAccess::Write), denied);
    }
}

// Ядро проверяет равенство VA = PA + base, чтобы не скопировать данные из другого фрейма.
#[test]
fn the_snapshot_skips_mappings_that_are_not_linear_cached_or_kernel_only() {
    let foreign = RAM_BLOCK + 8 * BLOCK_SIZE;
    let device = RAM_BLOCK + BLOCK_SIZE;
    let user_visible = device + BLOCK_SIZE;
    let linear = capture(&kernel_space(&[
        (LINEAR_OFFSET + RAM_BLOCK, foreign, KernelData::flags()),
        (LINEAR_OFFSET + device, device, Mmio::flags()),
        (
            LINEAR_OFFSET + user_visible,
            user_visible,
            user_flags(Access::UserRW, MemoryAttribute::Cached),
        ),
    ]))
    .unwrap();

    for frame in [RAM_BLOCK, foreign, device, user_visible] {
        assert_eq!(linear_alias(&linear, frame, UserAccess::Read), None);
    }
}

#[test]
fn the_snapshot_covers_only_the_window() {
    let kernel = kernel_space(&[
        (
            LINEAR_OFFSET - BLOCK_SIZE,
            UNMAPPED_FRAME,
            KernelData::flags(),
        ),
        (LINEAR_OFFSET + RAM_BLOCK, RAM_BLOCK, KernelData::flags()),
        (
            LINEAR_OFFSET + LINEAR_WINDOW,
            LINEAR_WINDOW,
            KernelData::flags(),
        ),
    ]);
    let linear = LinearMap::capture(
        &kernel,
        PageAlignedVirtualAddress::from_usize(LINEAR_OFFSET).unwrap(),
        RAM_BLOCK + 0x1000,
    )
    .unwrap();

    for frame in [UNMAPPED_FRAME, RAM_BLOCK + 0x1000, LINEAR_WINDOW] {
        assert_eq!(linear_alias(&linear, frame, UserAccess::Read), None);
    }
    assert_eq!(
        linear_alias(&linear, RAM_BLOCK, UserAccess::Read),
        Some(LINEAR_OFFSET + RAM_BLOCK)
    );
}

#[test]
fn a_mixed_tree_preserves_linear_ranges_permissions_and_holes() {
    let mut kernel = kernel_space(&[(LINEAR_OFFSET + RAM_BLOCK, RAM_BLOCK, KernelData::flags())]);
    let block = 0x8000_0000;
    kernel
        .map_page(
            AlignedVirtualAddress::<{ L1::SHIFT }>::from_usize(LINEAR_OFFSET + block).unwrap(),
            AlignedPhysicalAddress::<{ L1::SHIFT }>::from_usize(block).unwrap(),
            KernelData::flags(),
        )
        .unwrap();
    let page = RAM_BLOCK + 2 * BLOCK_SIZE;
    kernel
        .map_page(
            PageAlignedVirtualAddress::from_usize(LINEAR_OFFSET + page).unwrap(),
            AlignedPhysicalAddress::<{ L3::SHIFT }>::from_usize(page).unwrap(),
            KernelRoData::flags(),
        )
        .unwrap();
    let linear = capture(&kernel).unwrap();

    for (frame, access) in [
        (RAM_FRAME, UserAccess::Write),
        (block + 0x1234, UserAccess::Write),
        (page + 0x123, UserAccess::Read),
    ] {
        assert_eq!(
            linear_alias(&linear, frame, access),
            Some(LINEAR_OFFSET + frame)
        );
        assert_eq!(
            kernel
                .translate(VirtualAddress::new(LINEAR_OFFSET + frame))
                .unwrap()
                .pa,
            PhysicalAddress::new(frame)
        );
    }
    assert_eq!(
        linear_alias(&linear, RAM_BLOCK + BLOCK_SIZE, UserAccess::Read),
        None
    );
    assert_eq!(linear_alias(&linear, page, UserAccess::Write), None);
}

#[test]
fn adjacent_blocks_fit_the_snapshot_but_too_many_disjoint_ranges_do_not() {
    for (stride, expected) in [
        (BLOCK_SIZE, None),
        (2 * BLOCK_SIZE, Some(LinearMapError::TooManyRanges)),
    ] {
        let blocks: Vec<_> = (0..=LINEAR_RANGES_MAX)
            .map(|index| {
                let frame = RAM_BLOCK + stride * index;
                (LINEAR_OFFSET + frame, frame, KernelData::flags())
            })
            .collect();
        assert_eq!(capture(&kernel_space(&blocks)).err(), expected);
    }
}
