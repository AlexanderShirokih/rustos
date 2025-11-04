use crate::fdt::DeviceTree;
use crate::memory::entry_flags::EntryFlags;
use crate::memory::layout::{MemoryLayout, MemoryRegion};
use memory::physical::{PageAlignedAddress, PhysicalAddress};

const DEFAULT_FRAME_SIZE: usize = 4096;

pub(crate) struct MemoryLayoutBuilder {
    pub device_tree: DeviceTree,
    pub kernel_start: usize,
    pub kernel_end: usize,
}

impl From<MemoryLayoutBuilder> for MemoryLayout {
    fn from(memory_layout_builder: MemoryLayoutBuilder) -> Self {
        let dt = memory_layout_builder.device_tree;
        let (ram_start, ram_size) = find_primary_ram_region(&dt).unwrap_or((0, 0));

        let frame_size = DEFAULT_FRAME_SIZE;
        let kernel_start =
            PhysicalAddress::new(memory_layout_builder.kernel_start).align_down(frame_size);
        let kernel_end =
            PhysicalAddress::new(memory_layout_builder.kernel_end).align_up(frame_size);

        let kernel_region = MemoryRegion {
            label: "kernel",
            start: kernel_start,
            end: kernel_end,
            flags: EntryFlags::KERNEL_DATA,
            frame_size,
        };

        // Создаем DTB регион
        let dtb_size = dt.size();
        let dtb_start = PhysicalAddress::new(dt.base_address()).align_down(frame_size);
        let dtb_end = PhysicalAddress::new(dt.base_address() + dtb_size).align_up(frame_size);

        let dtb_region = MemoryRegion {
            label: "dtb",
            start: dtb_start,
            end: dtb_end,
            flags: EntryFlags::KERNEL_DATA,
            frame_size,
        };

        let heap_region = build_heap_region(ram_start, ram_size, frame_size);

        MemoryLayout {
            kernel: kernel_region,
            dtb: dtb_region,
            heap: heap_region,
        }
    }
}

fn build_heap_region(
    ram_start: usize,
    ram_size: usize,
    frame_size: usize,
) -> MemoryRegion<PageAlignedAddress> {
    let ram_end = ram_start.saturating_add(ram_size);

    let start = PhysicalAddress::new(ram_start).align_up(frame_size);
    let end = PhysicalAddress::new(ram_end).align_down(frame_size);

    MemoryRegion {
        label: "heap",
        start,
        end,
        flags: EntryFlags::NORMAL_MEMORY,
        frame_size,
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
                    return Some((start as usize, size as usize));
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
