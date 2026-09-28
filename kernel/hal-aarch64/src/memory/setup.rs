//! Построение раскладки памяти из DeviceTree.

use fdt::{
    devicetree::DeviceTree,
    devicetreeext::{find_ram_regions, reserved_memory_ranges},
};
use hal_aarch64_paging::preset::{KernelData, KernelRoData, KernelText};
use hal_common::boot::BootPayloadRange;

use crate::memory::layout::{
    LinkTimeRange, MemoryLayout, MemoryRegion, RegionTag, TooManyMemoryRegions,
};

unsafe extern "C" {
    static _text_start: u8;
    static _text_end: u8;

    static _rodata_start: u8;
    static _rodata_end: u8;

    static _rw_start: u8;
    static _rw_end: u8;
}

pub(crate) struct MemoryLayoutBuildError;

impl From<TooManyMemoryRegions> for MemoryLayoutBuildError {
    fn from(_: TooManyMemoryRegions) -> Self {
        Self
    }
}

pub(crate) fn build_memory_layout(
    dt: &DeviceTree,
    userland_blob: Option<BootPayloadRange>,
) -> Result<MemoryLayout, MemoryLayoutBuildError> {
    let mut layout = MemoryLayout::new();

    let ram_regions = find_ram_regions(dt).ok_or(MemoryLayoutBuildError)?;

    for ram_region in ram_regions {
        layout.add(MemoryRegion::new(
            RegionTag::Heap,
            ram_region.start(),
            ram_region.end(),
            KernelData::flags(),
        ))?;
    }

    // Device tree
    layout.add(MemoryRegion::new(
        RegionTag::Other,
        dt.base_address(),
        dt.base_address() + dt.size(),
        KernelRoData::flags(),
    ))?;

    // Userland blob (initrd)
    if let Some(range) = userland_blob {
        layout.add(MemoryRegion::new(
            RegionTag::Other,
            range.start().as_usize(),
            range.end_exclusive().as_usize(),
            KernelRoData::flags(),
        ))?;
    }

    // Блок резервирования памяти из FDT.
    for reservation in dt.memory_reservations() {
        let Some(end) = reservation.address.checked_add(reservation.size) else {
            continue;
        };
        if reservation.size == 0 {
            continue;
        }
        layout.add(MemoryRegion::new(
            RegionTag::Other,
            reservation.address,
            end,
            KernelRoData::flags(),
        ))?;
    }

    // Статические reg-диапазоны под /reserved-memory.
    if let Some(reserved_regions) = reserved_memory_ranges(dt) {
        for reserved in reserved_regions {
            let tag = if reserved.no_map {
                RegionTag::Unmapped
            } else {
                RegionTag::Other
            };

            layout.add(MemoryRegion::new(
                tag,
                reserved.range.start(),
                reserved.range.end(),
                KernelRoData::flags(),
            ))?;
        }
    }

    // SAFETY: `build_memory_layout` вызывается только из pre-MMU фазы, где
    // значение символа линкера равно физическому адресу секции.
    let sections = unsafe {
        [
            (
                LinkTimeRange::physical(&_text_start, &_text_end),
                KernelText::flags(),
            ),
            (
                LinkTimeRange::physical(&_rw_start, &_rw_end),
                KernelData::flags(),
            ),
            (
                LinkTimeRange::physical(&_rodata_start, &_rodata_end),
                KernelRoData::flags(),
            ),
        ]
    };

    for (range, flags) in sections {
        layout.add(range.into_region(RegionTag::Kernel, flags))?;
    }

    Ok(layout)
}
