use crate::aligned::Address;
use crate::frame::Frame;
use crate::physical_address::PageAlignedAddress;

#[derive(Copy, Clone, Debug)]
pub struct MemoryRange<A: Address> {
    from_inclusive: A,
    to_inclusive: A,
}

impl<A: Address> MemoryRange<A> {
    pub const fn new(start: A, end: A) -> Self {
        Self {
            from_inclusive: start,
            to_inclusive: end,
        }
    }

    pub const fn start(&self) -> A {
        self.from_inclusive
    }

    pub const fn end(&self) -> A {
        self.to_inclusive
    }

    pub fn contains(&self, addr: A) -> bool {
        self.from_inclusive <= addr && addr <= self.to_inclusive
    }

    pub fn size(&self) -> usize {
        self.end().as_usize() - self.start().as_usize()
    }
}

impl MemoryRange<PageAlignedAddress> {
    pub fn frame_count(&self) -> usize {
        let start_frame = Frame::from(self.start()).number();
        let end_frame = Frame::from(self.end()).number();

        end_frame - start_frame + 1
    }

    pub fn iter(&self) -> MemoryRangeIter {
        MemoryRangeIter::new(self.start(), self.end())
    }
}

pub struct MemoryRangeIter {
    current: PageAlignedAddress,
    end: PageAlignedAddress,
}

impl MemoryRangeIter {
    const fn new(start: PageAlignedAddress, end: PageAlignedAddress) -> Self {
        Self {
            current: start,
            end,
        }
    }
}

impl Iterator for MemoryRangeIter {
    type Item = PageAlignedAddress;

    fn next(&mut self) -> Option<Self::Item> {
        if self.current > self.end {
            return None;
        }

        let value = self.current;
        self.current = self.current.next_aligned();
        Some(value)
    }
}
