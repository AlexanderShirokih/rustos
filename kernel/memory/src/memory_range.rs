use crate::{aligned::Address, frame::Frame, physical_address::PageAlignedAddress};

/// Полуоткрытый диапазон адресов [start, end).
#[derive(Copy, Clone, Debug)]
pub struct MemoryRange<A: Address> {
    /// Начальный адрес (включительно).
    from_inclusive: A,
    /// Конечный адрес (исключительно).
    to_exclusive: A,
}

impl<A: Address> MemoryRange<A> {
    pub const fn new(start: A, end: A) -> Self {
        Self {
            from_inclusive: start,
            to_exclusive: end,
        }
    }

    pub const fn start(&self) -> A {
        self.from_inclusive
    }

    pub const fn end(&self) -> A {
        self.to_exclusive
    }

    pub fn contains(&self, addr: A) -> bool {
        self.from_inclusive <= addr && addr < self.to_exclusive
    }

    pub fn size(&self) -> usize {
        let start = self.start().as_usize();
        let end = self.end().as_usize();
        end.saturating_sub(start)
    }
}

impl MemoryRange<PageAlignedAddress> {
    pub fn frame_count(&self) -> usize {
        let start_frame = Frame::from(self.start()).number();
        let end_frame = Frame::from(self.end()).number();

        end_frame - start_frame
    }

    pub fn iter(&self) -> MemoryRangeIter {
        MemoryRangeIter::new(self.start(), self.end())
    }
}

impl IntoIterator for &MemoryRange<PageAlignedAddress> {
    type Item = PageAlignedAddress;
    type IntoIter = MemoryRangeIter;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

/// Итератор по выровненным адресам в диапазоне.
pub struct MemoryRangeIter {
    /// Текущий адрес итерации.
    current: PageAlignedAddress,
    /// Конечный адрес (исключительно).
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
        if self.current >= self.end {
            return None;
        }

        let value = self.current;
        self.current = self.current.next_aligned();
        Some(value)
    }
}
