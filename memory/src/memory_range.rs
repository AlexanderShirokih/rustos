use crate::physical::{Frame, PhysicalAddress};

pub struct MemoryRange {
    pub start: PhysicalAddress,
    pub end: PhysicalAddress,
    pub frame_size: usize,
}

impl MemoryRange {
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
