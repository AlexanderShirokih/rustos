use crate::aligned::Address;
use crate::frame::Frame;
use crate::physical_address::PageAlignedAddress;
use core::ops::RangeInclusive;

#[derive(Clone, Debug)]
pub struct MemoryRange<A: Address> {
    pub range: RangeInclusive<A>,
}

impl MemoryRange<PageAlignedAddress> {
    pub fn frame_count(&self) -> usize {
        let start_frame = Frame::from(self.start()).number();
        let end_frame = Frame::from(self.end()).number();

        end_frame - start_frame + 1
    }
}

impl<A: Address> MemoryRange<A> {
    pub const fn new(start: A, end: A) -> Self {
        Self { range: start..=end }
    }

    pub const fn start(&self) -> &A {
        self.range.start()
    }

    pub const fn end(&self) -> &A {
        self.range.end()
    }

    pub fn contains(&self, addr: A) -> bool {
        self.range.contains(&addr)
    }

    pub fn size(&self) -> usize {
        self.end().as_usize() - self.start().as_usize()
    }
}
