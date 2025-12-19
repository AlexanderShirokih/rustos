use crate::memory::layout::{MemoryLayout, MemoryRegion};
use aarch64_paging::preset::{KernelData, KernelRoData, KernelText};
use core::cmp::{max, min};
use fdt::devicetree::DeviceTree;
use fdt::devicetreeext::{AddressSpace, NodeExt, PropExt};
use memory::bump_allocator::BumpAllocator;
use memory::frame::Frame;

unsafe extern "C" {
    /** Код ядра */
    static _text_start: u8;
    static _text_end: u8;

    /** Статика ядра */
    static _rodata_start: u8;
    static _rodata_end: u8;

    /** Данные ядра */
    static _rw_start: u8;
    static _rw_end: u8;
}

pub(crate) struct MemoryLayoutBuildError;

/// Строит раскладку памяти на основе device tree
pub(crate) fn build_memory_layout<'a>(
    dt: &DeviceTree,
) -> Result<MemoryLayout, MemoryLayoutBuildError> {
    let mut layout = MemoryLayout::new();

    let ram_regions = find_ram_regions(&dt).ok_or(MemoryLayoutBuildError)?;

    for ram_region in ram_regions {
        layout.add(MemoryRegion::new(
            MemoryRegion::HEAP,
            ram_region.start(),
            ram_region.end(),
            KernelData::flags(),
            false,
        ));
    }

    layout.add(MemoryRegion::new(
        "Device tree",
        dt.base_address(),
        dt.base_address() + dt.size(),
        KernelData::flags(),
        true,
    ));

    unsafe {
        layout.add(MemoryRegion::new_raw(
            "Kernel code",
            &_text_start,
            &_text_end,
            KernelText::flags(),
            true,
        ));

        layout.add(MemoryRegion::new_raw(
            "Kernel data",
            &_rw_start,
            &_rw_end,
            KernelData::flags(),
            true,
        ));

        layout.add(MemoryRegion::new_raw(
            "Kernel read-only data",
            &_rodata_start,
            &_rodata_end,
            KernelRoData::flags(),
            true,
        ));

        Ok(layout)
    }
}

fn find_ram_regions(dt: &DeviceTree) -> Option<impl Iterator<Item = AddressSpace>> {
    let root = dt.root()?;

    Some(
        root.children()
            .filter(|node| {
                node.prop("device_type")
                    .and_then(|device_type| {
                        device_type.as_cstr().map(|str| str.starts_with("memory"))
                    })
                    .unwrap_or_else(|| node.name().starts_with("memory"))
            })
            .filter_map(|node| node.prop("reg"))
            .filter_map(move |prop| {
                let cells_size = root.cells_size().unwrap_or_default();
                let offsets_array = prop.try_as_reg_list::<1>(cells_size);

                offsets_array.and_then(|array| array.get(0).copied())
            })
            .filter(|offset_size| offset_size.size > 0),
    )
}

const MAX_REGIONS: usize = 32;

#[derive(Clone, Copy)]
struct FrameInterval {
    start: usize,
    end: usize,
}

impl FrameInterval {
    const fn new(start: usize, end: usize) -> Self {
        Self { start, end }
    }

    fn size(&self) -> usize {
        self.end.saturating_sub(self.start)
    }
}

pub(crate) fn create_bump_allocator(layout: &MemoryLayout) -> Result<BumpAllocator, ()> {
    let mut free_regions: collections::Vec<FrameInterval, MAX_REGIONS> = collections::Vec::new();

    layout.heap().for_each(|heap| {
        let heap_start = Frame::from(&heap.start).number();
        let heap_end = Frame::from(&heap.end).number();

        // Собираем свободные регионы для данного heap
        collect_free_regions(&layout, &mut free_regions, heap_start, heap_end);
    });

    // Находим самую большую свободную область
    let largest = free_regions
        .iter()
        .max_by_key(|interval| interval.size())
        .ok_or(())?;

    Ok(BumpAllocator::new(
        Frame::new(largest.start),
        Frame::new(largest.end),
    ))
}

/// Собирает свободные регионы в heap, вырезая исключённые области
fn collect_free_regions(
    layout: &MemoryLayout,
    free_regions: &mut collections::Vec<FrameInterval, MAX_REGIONS>,
    heap_start: usize,
    heap_end: usize,
) {
    // Сначала собираем и объединяем исключённые интервалы
    let mut excluded = collections::Vec::<FrameInterval, MAX_REGIONS>::new();

    for region in layout.iter().filter(|r| r.identity_map) {
        let start = Frame::from(&region.start).number();
        let end = Frame::from(&region.end).number();

        let clamped_start = max(start, heap_start);
        let clamped_end = min(end, heap_end);

        if clamped_start < clamped_end {
            let _ = excluded.push(FrameInterval::new(clamped_start, clamped_end));
        }
    }

    merge_intervals(&mut excluded);

    // Теперь находим свободные промежутки между исключёнными интервалами
    let mut cursor = heap_start;

    for interval in excluded.iter() {
        if cursor < interval.start {
            let _ = free_regions.push(FrameInterval::new(cursor, interval.start));
        }
        cursor = max(cursor, interval.end);
    }

    // Область после последнего исключённого интервала
    if cursor < heap_end {
        let _ = free_regions.push(FrameInterval::new(cursor, heap_end));
    }
}

fn merge_intervals(intervals: &mut collections::Vec<FrameInterval, MAX_REGIONS>) {
    if intervals.is_empty() {
        return;
    }

    intervals.sort_unstable_by_key(|&interval| interval.start);

    let mut write_idx = 0;
    for i in 1..intervals.len() {
        let current = intervals[i];
        let last = intervals[write_idx];

        if current.start <= last.end {
            intervals[write_idx].end = max(last.end, current.end);
        } else {
            write_idx += 1;
            intervals[write_idx] = current;
        }
    }

    intervals.truncate(write_idx + 1);
}
