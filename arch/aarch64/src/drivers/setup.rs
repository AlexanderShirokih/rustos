use crate::fdt::DeviceTree;
use crate::memory::entry_flags::EntryFlags;
use crate::memory::layout::MemoryRegion;
use aarch64_paging::MemoryLayout;
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
    let (heap_start, heap_end) = find_primary_ram_region(&dt).ok_or(MemoryLayoutBuildError {
        message: "Can't extract memory nodes",
    })?;

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

fn find_primary_ram_region(dt: &DeviceTree) -> Option<(usize, usize)> {
    let root = dt.root()?;

    let address_cells = root
        .get_prop(b"#address-cells")
        .and_then(|v| read_be_u32(v, 0))
        .map(|v| v as usize)
        .unwrap_or(2);

    let size_cells = root
        .get_prop(b"#size-cells")
        .and_then(|v| read_be_u32(v, 0))
        .map(|v| v as usize)
        .unwrap_or(1);

    let mut children = root.children();
    while let Some(node) = children.next() {
        let is_memory = node
            .get_prop(b"device_type")
            .map(|v| starts_with_str(v, b"memory"))
            .unwrap_or_else(|| node.name().starts_with(b"memory"));

        if !is_memory {
            continue;
        }

        if let Some(reg) = node.get_prop(b"reg") {
            if let Some((start, size)) = parse_first_reg(reg, address_cells, size_cells) {
                if size != 0 {
                    return Some((start as usize, (start + size) as usize));
                }
            }
        }
    }

    None
}

fn parse_first_reg(data: &[u8], address_cells: usize, size_cells: usize) -> Option<(u64, u64)> {
    let stride = (address_cells + size_cells) * 4;
    if stride == 0 || data.len() < stride {
        return None;
    }

    let mut addr: u64 = 0;
    for cell in 0..address_cells {
        let off = cell * 4;
        let value = read_be_u32(data, off)? as u64;
        addr = (addr << 32) | value;
    }

    let mut size: u64 = 0;
    for cell in 0..size_cells {
        let off = (address_cells + cell) * 4;
        let value = read_be_u32(data, off)? as u64;
        size = (size << 32) | value;
    }

    Some((addr, size))
}

fn read_be_u32(data: &[u8], offset: usize) -> Option<u32> {
    if offset + 4 > data.len() {
        return None;
    }

    let b0 = data[offset] as u32;
    let b1 = data[offset + 1] as u32;
    let b2 = data[offset + 2] as u32;
    let b3 = data[offset + 3] as u32;

    Some((b0 << 24) | (b1 << 16) | (b2 << 8) | b3)
}

fn starts_with_str(buf: &[u8], needle: &[u8]) -> bool {
    let until_nul = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    let slice = &buf[..until_nul];
    if slice.len() < needle.len() {
        return false;
    }
    slice[..needle.len()] == needle[..]
}
