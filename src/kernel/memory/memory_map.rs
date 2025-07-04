use crate::kernel::memory::physical::{Frame, PhysicalAddress};

/// MemoryMap describes physical memory boundaries
pub struct MemoryMap {
    /// Start of the stack
    stack: MemoryRegion,
    /// Start of the kernel code
    kernel: MemoryRegion,
    /// Start of available memory
    memory: MemoryRegion,
}

/// Represents a contiguous region of physical memory
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct MemoryRegion {
    pub start: PhysicalAddress,
    pub end: PhysicalAddress,
    pub frame_size: usize,
}

impl MemoryRegion {
    pub fn new(start: PhysicalAddress, end: PhysicalAddress, frame_size: usize) -> Self {
        Self {
            start,
            end,
            frame_size,
        }
    }

    pub fn frames_count(&self) -> usize {
        let start_frame = Frame::containing_address(self.start, self.frame_size).number();
        let end_frame = Frame::containing_address(self.end, self.frame_size).number();

        end_frame - start_frame + 1
    }

    pub fn size(&self) -> usize {
        self.end.0 - self.start.0 + 1
    }
}

impl MemoryMap {
    /// Create a new MemoryMap from the memory layout
    pub fn new(stack: MemoryRegion, kernel: MemoryRegion, memory: MemoryRegion) -> Self {
        Self {
            stack,
            kernel,
            memory,
        }
    }

    pub fn stack(&self) -> &MemoryRegion {
        &self.stack
    }

    pub fn kernel(&self) -> &MemoryRegion {
        &self.kernel
    }

    pub fn memory(&self) -> &MemoryRegion {
        &self.memory
    }
}
