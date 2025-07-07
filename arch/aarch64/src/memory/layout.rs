use crate::memory::entry_flags::EntryFlags;
use core::ptr::addr_of;
use memory::physical::PhysicalAddress;

// Memory layout constants from a linker script
unsafe extern "C" {
    static _ram_start: u8;
    static _ram_end: u8;
    static _stack_top: u8;
    static _stack_bottom: u8;
    static _kernel_start: u8;
    static _kernel_end: u8;
    static _device_memory_start: u8;
    static _device_memory_end: u8;
}

// Physical memory layout
pub struct MemoryLayout {
    pub devices: MemoryRegion,
    pub kernel: MemoryRegion,
    pub stack: MemoryRegion,
    pub heap: MemoryRegion,
}

pub struct MemoryRegion {
    pub label: &'static str,
    pub start: PhysicalAddress,
    pub end: PhysicalAddress,
    pub flags: EntryFlags,
    pub frame_size: usize,
}

impl MemoryLayout {
    pub unsafe fn get() -> Self {
        // TODO: Setup memory layout from DTB
        let alignment = 0x1000;

        let device_memory_start = PhysicalAddress(addr_of!(_device_memory_start) as usize);
        let device_memory_end = PhysicalAddress(addr_of!(_device_memory_end) as usize);

        let kernel_start = PhysicalAddress(addr_of!(_kernel_start) as usize);
        let kernel_end = PhysicalAddress(addr_of!(_kernel_end) as usize);

        let stack_start = PhysicalAddress(addr_of!(_stack_bottom) as usize);
        let stack_end = PhysicalAddress(addr_of!(_stack_top) as usize);

        let ram_end = PhysicalAddress(addr_of!(_ram_end) as usize);

        let heap_start = PhysicalAddress(Self::align_up(kernel_end.0, alignment)); // 4KB aligned
        let heap_end = if stack_start.0 > heap_start.0 {
            PhysicalAddress(stack_end.0) // Heap until stack
        } else {
            PhysicalAddress(ram_end.0) // Heap until the end of RAM
        };

        MemoryLayout {
            devices: MemoryRegion {
                label: "devices (MMIO)",
                start: device_memory_start,
                end: device_memory_end,
                flags: EntryFlags::DEVICE,
                frame_size: alignment,
            },
            stack: MemoryRegion {
                label: "kernel stack",
                start: stack_start,
                end: stack_end,
                flags: EntryFlags::KERNEL_RW,
                frame_size: alignment,
            },
            kernel: MemoryRegion {
                label: "kernel",
                start: kernel_start,
                end: kernel_end,
                flags: EntryFlags::KERNEL_RW,
                frame_size: alignment,
            },
            heap: MemoryRegion {
                label: "heap",
                start: heap_start,
                end: heap_end,
                flags: EntryFlags::KERNEL_RW,
                frame_size: alignment,
            },
        }
    }

    fn align_up(addr: usize, align: usize) -> usize {
        (addr + align - 1) & !(align - 1)
    }
}
