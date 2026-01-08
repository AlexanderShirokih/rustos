use crate::frame::Frame;
use crate::memory_range::MemoryRange;
use crate::physical_address::PhysicalAddress;
use core::alloc::Layout;
use core::fmt::Formatter;
use core::ptr::NonNull;

pub struct BumpAllocator {
    start: usize,
    end: usize,
    offset: usize,
}

impl BumpAllocator {
    pub const fn new(from: Frame, to: Frame) -> Self {
        Self {
            start: from.page_address().as_usize(),
            end: to.page_address().as_usize(),
            offset: 0,
        }
    }

    fn remaining(&self) -> usize {
        self.end - self.start - self.offset
    }

    pub fn get_used_area(&self) -> MemoryRange<PhysicalAddress> {
        MemoryRange::new(
            PhysicalAddress::new(self.start),
            PhysicalAddress::new(self.start + self.offset),
        )
    }

    pub fn allocate(&mut self, layout: Layout) -> Result<NonNull<u8>, BumpAllocError> {
        let align = layout.align();
        let size = layout.size();
        let base = self.start;
        let current = base + self.offset;
        let aligned = current.div_ceil(align) * align;

        let new_offset = (aligned - base)
            .checked_add(size)
            .ok_or(BumpAllocError::AddressOverflow)?;

        if base + new_offset > self.end {
            return Err(BumpAllocError::OutOfMemory {
                required_size: size,
                available_size: self.remaining(),
            });
        }

        self.offset = new_offset;

        unsafe { Ok(NonNull::new_unchecked(aligned as *mut u8)) }
    }
}

pub enum BumpAllocError {
    AddressOverflow,

    OutOfMemory {
        required_size: usize,
        available_size: usize,
    },
}

impl core::fmt::Display for BumpAllocError {
    fn fmt(&self, f: &mut Formatter<'_>) -> core::fmt::Result {
        match self {
            BumpAllocError::AddressOverflow => write!(f, "Address overflow"),

            BumpAllocError::OutOfMemory {
                required_size,
                available_size,
            } => {
                write!(
                    f,
                    "Out of memory: required {} bytes, available {} bytes",
                    required_size, available_size
                )
            }
        }
    }
}
