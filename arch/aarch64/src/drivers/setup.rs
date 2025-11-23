use crate::memory::entry_flags::EntryFlags;
use crate::memory::layout::MemoryRegion;
use aarch64_paging::MemoryLayout;
use fdt::devicetree::DeviceTree;
use fdt::devicetreeext::{DeviceTreeExt, OffsetSize, PropExt};
use memory::physical::PageAlignedAddress;

unsafe extern "C" {
    static _text_start: u8;
    static _text_end: u8;

    static _rodata_start: u8;
    static _rodata_end: u8;

    static _rw_start: u8;
    static _rw_end: u8;
}

pub(crate) struct MemoryLayoutBuildError {
    pub message: &'static str,
}

pub(crate) fn build_memory_layout(
    dt: &DeviceTree,
    additional: MemoryRegion<PageAlignedAddress>,
) -> Result<MemoryLayout, MemoryLayoutBuildError> {
    let offset_size = find_primary_ram_region(&dt).ok_or(MemoryLayoutBuildError {
        message: "Can't extract memory nodes",
    })?;

    let (heap_start, heap_end) = (offset_size.start(), offset_size.end());
    let heap_region = MemoryRegion::new("Heap", heap_start, heap_end, EntryFlags::KERNEL_DATA);

    let device_tree_region = MemoryRegion::new(
        "Device Tree",
        dt.base_address(),
        dt.base_address() + dt.size(),
        EntryFlags::KERNEL_DATA,
    );

    unsafe {
        let kernel_code_region = MemoryRegion::new_raw(
            "Kernel code",
            &_text_start,
            &_text_end,
            EntryFlags::KERNEL_CODE,
        );

        let kernel_data_region =
            MemoryRegion::new_raw("Kernel data", &_rw_start, &_rw_end, EntryFlags::KERNEL_DATA);

        let kernel_rodata_region = MemoryRegion::new_raw(
            "Kernel read-only data",
            &_rodata_start,
            &_rodata_end,
            EntryFlags::KERNEL_RODATA,
        );

        Ok(MemoryLayout {
            kernel_code: kernel_code_region,
            kernel_rodata: kernel_rodata_region,
            kernel_data: kernel_data_region,
            dtb: device_tree_region,
            heap: heap_region,
            additional,
        })
    }
}

fn find_primary_ram_region(dt: &DeviceTree) -> Option<OffsetSize> {
    let cells_size = dt.cells_size()?;

    dt.root()?
        .children()
        .filter(|node| {
            node.prop("device_type")
                .and_then(|device_type| device_type.as_cstr().map(|str| str.starts_with("memory")))
                .unwrap_or_else(|| node.name().starts_with("memory"))
        })
        .filter_map(|node| node.prop("reg"))
        .filter_map(|prop| prop.as_offset_size(cells_size))
        .find(|offset_size| offset_size.size > 0)
}
