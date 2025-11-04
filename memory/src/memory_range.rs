use crate::physical::{AddressType, Frame, PageAlignedAddress};
use core::ops::RangeInclusive;

#[derive(Clone, Debug)]
pub struct MemoryRange<A: AddressType> {
    pub range: RangeInclusive<A>,
    pub frame_size: usize,
}

impl MemoryRange<PageAlignedAddress> {
    pub fn frame_count(&self) -> usize {
        let start_frame = Frame::from(self.start()).number();
        let end_frame = Frame::from(self.end()).number();

        end_frame - start_frame + 1
    }
}

impl<A: AddressType> MemoryRange<A> {
    pub fn new(start: A, end: A, frame_size: usize) -> Self {
        Self {
            range: start..=end,
            frame_size,
        }
    }

    #[inline]
    pub fn start(&self) -> A {
        *self.range.start()
    }

    #[inline]
    pub fn end(&self) -> A {
        *self.range.end()
    }

    #[inline]
    pub fn contains(&self, addr: A) -> bool {
        self.range.contains(&addr)
    }

    pub fn size(&self) -> usize {
        self.end().as_usize() - self.start().as_usize()
    }

    /// Вычитает other из self и возвращает доступные поддиапазоны.
    pub fn subtract(&self, other: &MemoryRange<A>) -> AvailableRegions<A> {
        let (a0, a1) = (self.start(), self.end());
        let (b0, b1) = (other.start(), other.end());

        // нет пересечения
        if a1 < b0 || b1 < a0 {
            return AvailableRegions::One(self.clone());
        }

        // пересечение есть
        let left_start = a0;
        let left_end = b0;
        let right_start = b1;
        let right_end = a1;

        let has_left = left_start <= left_end;
        let has_right = right_start <= right_end;

        match (has_left, has_right) {
            (false, false) => AvailableRegions::None, // self целиком внутри other
            (true, false) => AvailableRegions::One(MemoryRange::new(
                left_start,
                left_end,
                self.frame_size,
            )),
            (false, true) => AvailableRegions::One(MemoryRange::new(
                right_start,
                right_end,
                self.frame_size,
            )),
            (true, true) => AvailableRegions::Two {
                left: MemoryRange::new(left_start, left_end, self.frame_size),
                right: MemoryRange::new(right_start, right_end, self.frame_size),
            },
        }
    }
}

#[derive(Debug)]
pub enum AvailableRegions<A: AddressType> {
    None,
    One(MemoryRange<A>),
    Two {
        left: MemoryRange<A>,
        right: MemoryRange<A>,
    },
}
