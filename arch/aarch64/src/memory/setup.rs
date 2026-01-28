use crate::memory::layout::{MemoryLayout, MemoryRegion};
use aarch64_paging::preset::{KernelData, KernelRoData, KernelText};
use fdt::devicetree::DeviceTree;
use fdt::devicetreeext::{AddressSpace, NodeExt, PropExt};

/// База верхней половины виртуального адресного пространства
pub const HIGHER_HALF_BASE: usize = 0xFFFF_FF80_0000_0000;

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
pub(crate) fn build_memory_layout(
    dt: &DeviceTree,
) -> Result<MemoryLayout, MemoryLayoutBuildError> {
    let mut layout = MemoryLayout::new();

    let ram_regions = find_ram_regions(&dt).ok_or(MemoryLayoutBuildError)?;

    // RAM маппится дважды:
    // 1. Identity (для bump памяти после MMU)
    // 2. Higher half (для heap)
    for ram_region in ram_regions {
        // Identity mapping (VA == PA) — для доступа к bump памяти после MMU
        layout.add(MemoryRegion::new(
            "RAM (identity)",
            ram_region.start(),
            ram_region.end(),
            KernelData::flags(),
            0, // identity: va_offset = 0
        ));

        // Higher half mapping — для heap
        layout.add(MemoryRegion::new(
            MemoryRegion::HEAP,
            ram_region.start(),
            ram_region.end(),
            KernelData::flags(),
            HIGHER_HALF_BASE,
        ));
    }

    // Device tree — identity mapping
    layout.add(MemoryRegion::identity(
        "Device tree",
        dt.base_address(),
        dt.base_address() + dt.size(),
        KernelData::flags(),
    ));

    unsafe {
        // Kernel секции — identity mapping
        layout.add(MemoryRegion::identity_raw(
            "Kernel code",
            &_text_start,
            &_text_end,
            KernelText::flags(),
        ));

        layout.add(MemoryRegion::identity_raw(
            "Kernel data",
            &_rw_start,
            &_rw_end,
            KernelData::flags(),
        ));

        layout.add(MemoryRegion::identity_raw(
            "Kernel read-only data",
            &_rodata_start,
            &_rodata_end,
            KernelRoData::flags(),
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
