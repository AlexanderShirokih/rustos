extern crate alloc;

use alloc::vec::Vec;

use hal_aarch64_paging::preset::KernelData;
use kernel_tests::kernel_test;

use crate::memory::layout::{MemoryLayout, MemoryRegion, RegionTag};

#[kernel_test]
fn free_heap_regions_exclude_unmapped_regions() {
    let mut layout = MemoryLayout::new();

    for (tag, start, end) in [
        (RegionTag::Heap, 0x4000_0000, 0x4100_0000),
        (RegionTag::Unmapped, 0x4040_0000, 0x4050_0000),
        (RegionTag::Other, 0x4080_0000, 0x4081_0000),
    ] {
        layout
            .add(MemoryRegion::new(tag, start, end, KernelData::flags()))
            .expect("layout has room for three regions");
    }

    let free: Vec<_> = layout
        .free_heap_regions()
        .expect("free heap regions fit the interval set")
        .iter()
        .map(|interval| (interval.start.as_usize(), interval.end.as_usize()))
        .collect();

    kernel_tests::kassert_eq!(
        free,
        [
            (0x4000_0000, 0x4040_0000),
            (0x4050_0000, 0x4080_0000),
            (0x4081_0000, 0x4100_0000),
        ]
    );
}

#[kernel_test]
fn mapped_regions_exclude_unmapped_intervals_except_kernel() {
    let mut layout = MemoryLayout::new();

    for (tag, start, end) in [
        (RegionTag::Heap, 0x4000_0000, 0x4100_0000),
        (RegionTag::Unmapped, 0x4040_0000, 0x4050_0000),
        (RegionTag::Other, 0x403f_0000, 0x4041_0000),
        (RegionTag::Other, 0x4042_0000, 0x4043_0000),
        (RegionTag::Other, 0x404f_0000, 0x4051_0000),
        (RegionTag::Kernel, 0x404a_0000, 0x404b_0000),
    ] {
        layout
            .add(MemoryRegion::new(tag, start, end, KernelData::flags()))
            .expect("layout has room for six regions");
    }

    let mut mapped = io::Vec::new();
    layout
        .push_mapped_non_heap_regions(&mut mapped)
        .expect("mapped regions fit the region list");

    let mapped: Vec<_> = mapped
        .iter()
        .map(|region| (region.tag, region.start.as_usize(), region.end.as_usize()))
        .collect();

    kernel_tests::kassert_eq!(
        mapped,
        [
            (RegionTag::Other, 0x403f_0000, 0x4040_0000),
            (RegionTag::Other, 0x4050_0000, 0x4051_0000),
            (RegionTag::Kernel, 0x404a_0000, 0x404b_0000),
        ]
    );
}
