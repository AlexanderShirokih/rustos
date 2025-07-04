//! Physical memory management
//!
//! This module provides functionality for managing physical memory frames.

use crate::kernel::Kernel;
use crate::kernel::memory::memory_map::MemoryRegion;
use crate::kernel::util::log;
use crate::kernel::util::string::{usize_to_hex_str, usize_to_str};
use core::mem::size_of;
use core::ptr::NonNull;
use core::sync::atomic::{AtomicUsize, Ordering};
use spin::Mutex;

/// Physical memory address
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct PhysicalAddress(pub usize);

impl PhysicalAddress {
    /// Create a new physical address
    pub const fn new(address: usize) -> Self {
        PhysicalAddress(address)
    }

    /// Get the raw address value
    pub const fn as_usize(&self) -> usize {
        self.0
    }

    /// Align the address down to the frame boundary
    pub fn align_down(&self, frame_size: usize) -> PhysicalAddress {
        PhysicalAddress(self.0 & !(frame_size - 1))
    }

    /// Align the address up to the frame boundary
    pub fn align_up(&self, frame_size: usize) -> PhysicalAddress {
        let aligned = (self.0 + frame_size - 1) & !(frame_size - 1);
        PhysicalAddress(aligned)
    }

    pub fn offset_bytes(&self, bytes: usize) -> PhysicalAddress {
        PhysicalAddress(self.0 + bytes)
    }
}

/// A physical memory frame
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Frame {
    number: usize,
}

impl Frame {
    /// Create a new frame from a frame number
    pub const fn new(number: usize) -> Self {
        Frame { number }
    }

    /// Create a frame containing the given physical address
    pub fn containing_address(address: PhysicalAddress, frame_size: usize) -> Self {
        Frame {
            number: address.as_usize() / frame_size,
        }
    }

    /// Get the starting physical address of this frame
    pub fn start_address(&self, frame_size: usize) -> PhysicalAddress {
        PhysicalAddress(self.number * frame_size)
    }

    /// Get the frame number
    pub const fn number(&self) -> usize {
        self.number
    }
}

/// Frame allocator trait
pub trait FrameAllocator {
    fn frame_size(&self) -> usize;

    /// Allocate a frame
    fn allocate_frame(&mut self) -> Option<Frame>;

    /// Deallocate a frame
    fn deallocate_frame(&mut self, frame: Frame);
}

/// Bitmap to track frame allocation status
struct FrameBitmap {
    // Pointer to the bitmap memory
    bitmap_ptr: NonNull<u64>,
    // Total number of frames that can be tracked
    total_frames: usize,
}

impl FrameBitmap {
    const ENTRY_SIZE: usize = size_of::<u64>();
    const FRAMES_PER_ENTRY: usize = FrameBitmap::ENTRY_SIZE * 8; // 64 frames per u64

    /// Create a new empty bitmap
    fn new(placement_address: PhysicalAddress, target_region: &MemoryRegion) -> Self {
        let total_frames = target_region.frames_count();

        // round up entries count
        let entries_count = Self::get_entries_count(total_frames);

        // Create a pointer to the bitmap memory
        let bitmap_ptr = NonNull::new(placement_address.as_usize() as *mut u64)
            .expect("Invalid bitmap placement address");

        // Initialize all bitmap entries to 0 (all frames free)
        unsafe {
            for i in 0..entries_count {
                *bitmap_ptr.as_ptr().add(i) = 0;
            }
        }

        FrameBitmap {
            bitmap_ptr,
            total_frames,
        }
    }

    pub(crate) fn size_for(region: &MemoryRegion) -> usize {
        Self::get_entries_count(region.frames_count()) * Self::ENTRY_SIZE
    }

    fn get_entries_count(frames_count: usize) -> usize {
        // round up
        (frames_count + Self::FRAMES_PER_ENTRY - 1) / Self::FRAMES_PER_ENTRY
    }

    /// Set a bit in the bitmap (mark as allocated)
    fn set(&mut self, frame_idx: usize) {
        if frame_idx < self.total_frames {
            let word_idx = frame_idx / Self::FRAMES_PER_ENTRY;
            let bit_idx = frame_idx % Self::FRAMES_PER_ENTRY;

            unsafe {
                let entry_ptr = self.bitmap_ptr.as_ptr().add(word_idx);
                *entry_ptr |= 1u64 << bit_idx;
            }
        }
    }

    /// Clear a bit in the bitmap (mark as free)
    fn clear(&mut self, frame_idx: usize) {
        if frame_idx < self.total_frames {
            let word_idx = frame_idx / Self::FRAMES_PER_ENTRY;
            let bit_idx = frame_idx % Self::FRAMES_PER_ENTRY;

            unsafe {
                let entry_ptr = self.bitmap_ptr.as_ptr().add(word_idx);
                *entry_ptr &= !(1u64 << bit_idx);
            }
        } else {
            // Out-of-range frames are considered allocated, so clearing them is a no-op
            // This makes the behavior consistent with is_set
        }
    }

    /// Check if a bit is set (frame is allocated)
    fn is_set(&self, frame_idx: usize) -> bool {
        if frame_idx < self.total_frames {
            let word_idx = frame_idx / Self::FRAMES_PER_ENTRY;
            let bit_idx = frame_idx % Self::FRAMES_PER_ENTRY;

            unsafe {
                let entry_ptr = self.bitmap_ptr.as_ptr().add(word_idx);
                (*entry_ptr & (1u64 << bit_idx)) != 0
            }
        } else {
            true // Out-of-range frames are considered allocated
        }
    }

    /// Find the first free frame
    fn find_free_frame(&self, start_idx: usize) -> Option<usize> {
        let mut idx = start_idx;

        // First search from start_idx to end
        while idx < self.total_frames {
            if !self.is_set(idx) {
                return Some(idx);
            }
            idx += 1;
        }

        // If we didn't find a free frame after start_idx, wrap around
        idx = 0;
        while idx < start_idx {
            if !self.is_set(idx) {
                return Some(idx);
            }
            idx += 1;
        }

        None // No free frames
    }
}

/// Physical memory manager
pub struct PhysicalMemoryManager {
    /// Single RAM memory region
    memory: MemoryRegion,
    /// Next frame to allocate
    next_frame: AtomicUsize,
    /// Bitmap of allocated frames
    allocated_frames: Mutex<FrameBitmap>,
}

impl PhysicalMemoryManager {
    /// Create a new physical memory manager
    pub fn new(memory: &MemoryRegion) -> Self {
        let frame_size = memory.frame_size;
        let start_frame = Frame::containing_address(memory.start.align_up(frame_size), frame_size);
        let end_frame = Frame::containing_address(memory.end.align_down(frame_size), frame_size);

        assert!(start_frame.number() <= end_frame.number());

        log::print("Expanding RAM region from 0x");
        log::print(usize_to_hex_str(start_frame.number()));
        log::print(" to ");
        log::print(usize_to_hex_str(end_frame.number()));
        log::print(" physical memory. Which is ");
        log::print(usize_to_str(memory.size() / 1024));
        log::print("KB total\n\r");

        // Create the aligned memory region for the manager
        let aligned_memory = MemoryRegion::new(
            start_frame.start_address(frame_size),
            end_frame.start_address(frame_size),
            frame_size,
        );

        // Calculate bytes needed for the bitmap based on the aligned memory region
        let bytes_to_alloc_bitmap = FrameBitmap::size_for(&aligned_memory);

        // Allocate bytes for frame_bitmap
        let ppm_self_region_begin = memory.start;
        let ppm_self_region_end = ppm_self_region_begin.offset_bytes(bytes_to_alloc_bitmap);
        let ppm_self_region =
            MemoryRegion::new(ppm_self_region_begin, ppm_self_region_end, frame_size);

        let manager = PhysicalMemoryManager {
            memory: aligned_memory,
            next_frame: AtomicUsize::new(0),
            allocated_frames: Mutex::new(FrameBitmap::new(ppm_self_region_begin, &aligned_memory)),
        };

        // Mark the bitmap region itself as used
        manager.mark_used_region(&ppm_self_region);

        manager
    }

    pub fn frame_size(&self) -> usize {
        self.memory.frame_size
    }

    /// Get the total number of frames
    pub fn total_frames(&self) -> usize {
        self.memory.frames_count()
    }

    /// Mark a range of memory as used
    pub fn mark_used_region(&self, region: &MemoryRegion) {
        let frame_size = self.memory.frame_size;
        let start_frame =
            Frame::containing_address(region.start.align_down(frame_size), frame_size);
        let end_frame = Frame::containing_address(region.end.align_up(frame_size), frame_size);
        let base_frame = Frame::containing_address(self.memory.start, frame_size).number();

        let mut bitmap = self.allocated_frames.lock();
        for frame_num in start_frame.number()..=end_frame.number() {
            if frame_num >= base_frame && frame_num < base_frame + self.total_frames() {
                let idx = frame_num - base_frame;
                bitmap.set(idx);
            }
        }
    }
}

impl FrameAllocator for PhysicalMemoryManager {
    fn frame_size(&self) -> usize {
        self.frame_size()
    }

    fn allocate_frame(&mut self) -> Option<Frame> {
        let mut bitmap = self.allocated_frames.lock();
        let start_frame_num =
            Frame::containing_address(self.memory.start, self.memory.frame_size).number();
        let total_frames = self.total_frames();

        // Start from the next_frame hint
        let current = self.next_frame.load(Ordering::Relaxed);

        if let Some(idx) = bitmap.find_free_frame(current) {
            bitmap.set(idx);
            self.next_frame
                .store((idx + 1) % total_frames, Ordering::Relaxed);
            return Some(Frame::new(start_frame_num + idx));
        }

        None // No free frames
    }

    fn deallocate_frame(&mut self, frame: Frame) {
        let start_frame_num =
            Frame::containing_address(self.memory.start, self.memory.frame_size).number();
        let frame_num = frame.number();

        // Check if the frame is within the valid range
        if frame_num >= start_frame_num && frame_num < start_frame_num + self.total_frames() {
            let idx = frame_num - start_frame_num;
            let mut bitmap = self.allocated_frames.lock();
            bitmap.clear(idx);
        } else {
            // Frame is outside the managed memory range
            log::print("Warning: Attempted to deallocate frame outside managed memory range\n\r");
        }
    }
}

/// Initialize the physical memory manager
pub(crate) fn init(kernel: &Kernel) -> PhysicalMemoryManager {
    let memory_map = kernel.boot_info().memory_map();

    // Create physical memory manager with available memory region
    let pmm = PhysicalMemoryManager::new(memory_map.memory());

    // Mark kernel and stack as used
    pmm.mark_used_region(memory_map.stack());
    pmm.mark_used_region(memory_map.kernel());

    pmm
}
