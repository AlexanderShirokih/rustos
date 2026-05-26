//! Построение раскладки памяти из DeviceTree.

use fdt::{
    devicetree::DeviceTree,
    devicetreeext::{AddressSpace, NodeExt, PropExt},
};
use hal_aarch64_paging::preset::{KernelData, KernelRoData, KernelText};
use hal_common::boot::BootPayloadRange;

use crate::memory::layout::{MemoryLayout, MemoryRegion, RegionTag};

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

/// Ошибка построения раскладки памяти.
pub(crate) struct MemoryLayoutBuildError;

/// Строит раскладку памяти из DeviceTree.
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
        ));
    }

    // Device tree
    layout.add(MemoryRegion::new(
        RegionTag::Other,
        dt.base_address(),
        dt.base_address() + dt.size(),
        KernelRoData::flags(),
    ));

    // Userland blob (initrd): резервируем по аналогии с DTB
    if let Some(range) = userland_blob {
        layout.add(MemoryRegion::new(
            RegionTag::Other,
            range.start().as_usize(),
            range.end_exclusive().as_usize(),
            KernelRoData::flags(),
        ));
    }

    // SAFETY: символы `_text_start`/`_text_end`/`_rodata_*`/`_rw_*` определены линкером и
    // указывают на границы соответствующих секций ядра - взятие `&` от них корректно.
    unsafe {
        layout.add(MemoryRegion::new_raw(
            RegionTag::Kernel,
            &_text_start,
            &_text_end,
            KernelText::flags(),
        ));

        layout.add(MemoryRegion::new_raw(
            RegionTag::Kernel,
            &_rw_start,
            &_rw_end,
            KernelData::flags(),
        ));

        layout.add(MemoryRegion::new_raw(
            RegionTag::Kernel,
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
            .flat_map(move |prop| {
                let cells_size = root.cells_size().unwrap_or_default();
                prop.try_as_reg_list::<8>(cells_size)
                    .unwrap_or_default()
                    .into_iter()
            })
            .filter(|offset_size| offset_size.size > 0),
    )
}
