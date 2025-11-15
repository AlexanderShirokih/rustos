use crate::memory_backend::MemoryBackend;
use crate::memory_range::MemoryRange;
use crate::physical::{Frame, PageAlignedAddress};
use core::cmp::{max, min};
use core::mem::size_of;

/// Битовая карта для отслеживания статуса выделения фреймов
pub struct FrameBitmap<'a, B: MemoryBackend> {
    // Интерфейс для обращения к физической памяти
    memory_backend: &'a B,

    // Область памяти, которая управляется битовой картой
    target_region: MemoryRange<PageAlignedAddress>,

    // Фрейм начала [target_region]
    base_frame: Frame,

    // Физический адрес, выделенный под хранение данного экземпляра FrameBitmap
    bitmap_address: PageAlignedAddress,

    bitmap_end_address: PageAlignedAddress,
}

#[derive(Debug)]
pub enum FrameBitmapError {
    TooManyRegions,
    UnableToAllocate,
}

const MAX_FORBIDDEN_INTERVALS: usize = 64;

#[derive(Clone, Copy)]
struct FrameInterval {
    start: usize,
    end: usize,
}

impl FrameInterval {
    const fn new(start: usize, end: usize) -> Self {
        Self { start, end }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct EntryPos {
    word: usize,
    bit: usize,
}

impl EntryPos {
    const fn new(word: usize, bit: usize) -> Self {
        Self { word, bit }
    }

    #[inline]
    fn is_before(self, other: EntryPos) -> bool {
        self.word < other.word || (self.word == other.word && self.bit < other.bit)
    }
}

impl<'a, B: MemoryBackend> FrameBitmap<'a, B> {
    // Количество байт, необходимых для одной записи в битовой карте
    const ENTRY_BYTES: usize = size_of::<u64>();

    // Количество фреймов (битов), описываемых одной записью
    const BITS_PER_ENTRY: usize = FrameBitmap::<'a, B>::ENTRY_BYTES * 8;

    pub fn new(
        memory_backend: &'a B,
        target_region: &MemoryRange<PageAlignedAddress>,
        excluded_regions: &[MemoryRange<PageAlignedAddress>],
    ) -> Result<Self, FrameBitmapError> {
        let required_frames = Self::calc_required_frames_for_bitmap(target_region);

        let forbidden = Self::collect_forbidden_intervals(target_region, excluded_regions)?;

        let bitmap_interval =
            Self::find_bitmap_interval(target_region, &forbidden, required_frames)
                .ok_or(FrameBitmapError::UnableToAllocate)?;
        let bitmap_start_frame = Frame::new(bitmap_interval.start);
        let bitmap_end_frame = Frame::new(bitmap_interval.end);

        let frame_bitmap = FrameBitmap {
            memory_backend,
            base_frame: Frame::from(target_region.start()),
            target_region: target_region.clone(),
            bitmap_address: bitmap_start_frame.page_address(),
            bitmap_end_address: bitmap_end_frame.page_address(),
        };

        // Очищаем физическую память для аллокатора (обнуляем всю битовую карту)
        frame_bitmap.clear_bitmap(required_frames);

        // Помечаем сам регион битовой карты как использованный
        if required_frames > 0 {
            frame_bitmap.set_range_unchecked(
                Frame::new(bitmap_interval.start),
                Frame::new(bitmap_interval.end),
            );
        }

        for exclude in excluded_regions {
            frame_bitmap.set_range_unchecked(
                Frame::from(exclude.start()),
                Frame::from(exclude.end()).add(1),
            )
        }

        Ok(frame_bitmap)
    }

    fn clear_bitmap(&self, required_frames: usize) {
        let entries = (required_frames + (Self::BITS_PER_ENTRY - 1)) / Self::BITS_PER_ENTRY;

        for i in 0..entries {
            let addr = self
                .bitmap_address
                .as_physical_address()
                .add(i * size_of::<u64>());
            self.memory_backend.write::<u64>(addr, 0);
        }
    }

    fn calc_required_frames_for_bitmap(region: &MemoryRange<PageAlignedAddress>) -> usize {
        let entry_count = region.frame_count().div_ceil(Self::BITS_PER_ENTRY);
        let bitmap_bytes = entry_count * Self::ENTRY_BYTES;
        bitmap_bytes.div_ceil(region.frame_size)
    }

    fn collect_forbidden_intervals(
        region: &MemoryRange<PageAlignedAddress>,
        excluded_regions: &[MemoryRange<PageAlignedAddress>],
    ) -> Result<heapless::Vec<FrameInterval, MAX_FORBIDDEN_INTERVALS>, FrameBitmapError> {
        let mut intervals: heapless::Vec<FrameInterval, MAX_FORBIDDEN_INTERVALS> =
            heapless::Vec::new();
        let region_start = Frame::from(region.start()).number();
        let region_end = Frame::from(region.end()).add(1).number();

        for exclude in excluded_regions {
            let start = Frame::from(exclude.start()).number();
            let end = Frame::from(exclude.end()).add(1).number();

            let clamped_start = max(start, region_start);
            let clamped_end = min(end, region_end);

            if clamped_start >= clamped_end {
                continue;
            }

            intervals
                .push(FrameInterval::new(clamped_start, clamped_end))
                .map_err(|_| FrameBitmapError::TooManyRegions)?;
        }

        Self::merge_intervals(&mut intervals);

        Ok(intervals)
    }

    fn merge_intervals(intervals: &mut heapless::Vec<FrameInterval, MAX_FORBIDDEN_INTERVALS>) {
        if intervals.is_empty() {
            return;
        }

        intervals.sort_unstable_by(|a, b| a.start.cmp(&b.start));

        let mut write_idx = 0;
        for i in 1..intervals.len() {
            let current = intervals[i];
            let last = intervals[write_idx];

            if current.start <= last.end {
                let merged_end = max(last.end, current.end);
                intervals[write_idx].end = merged_end;
            } else {
                write_idx += 1;
                intervals[write_idx] = current;
            }
        }

        intervals.truncate(write_idx + 1);
    }

    fn find_bitmap_interval(
        region: &MemoryRange<PageAlignedAddress>,
        forbidden: &[FrameInterval],
        required_frames: usize,
    ) -> Option<FrameInterval> {
        let region_start = Frame::from(region.start()).number();
        let region_end = Frame::from(region.end()).add(1).number();

        if required_frames == 0 {
            return Some(FrameInterval::new(region_start, region_start));
        }

        let mut cursor = region_start;

        for interval in forbidden {
            if interval.start > region_end {
                break;
            }

            if cursor < interval.start {
                let gap = interval.start - cursor;
                if gap >= required_frames {
                    return Some(FrameInterval::new(cursor, cursor + required_frames));
                }
            }

            cursor = max(cursor, interval.end);

            if cursor >= region_end {
                break;
            }
        }

        if cursor < region_end {
            let remaining = region_end - cursor;
            if remaining >= required_frames {
                return Some(FrameInterval::new(cursor, cursor + required_frames));
            }
        }

        None
    }

    #[inline]
    fn write<F: Fn(u64) -> u64>(&self, index: usize, update: F) {
        let addr = self
            .bitmap_address
            .as_physical_address()
            .add(index * Self::ENTRY_BYTES);

        let old = self.memory_backend.read::<u64>(addr);
        let new = update(old);
        self.memory_backend.write::<u64>(addr, new);
    }

    #[inline]
    fn read(&self, offset: usize) -> u64 {
        let address = self
            .bitmap_address
            .as_physical_address()
            .add(offset * Self::ENTRY_BYTES);

        self.memory_backend.read::<u64>(address)
    }

    pub fn get_alloc_range(&self) -> MemoryRange<PageAlignedAddress> {
        MemoryRange::new(
            self.bitmap_address,
            self.bitmap_end_address,
            PageAlignedAddress::alignment(),
        )
    }

    /// Помечает область фреймов как выделенную
    pub fn set_range_unchecked(&self, from_inclusive: Frame, to_exclusive: Frame) {
        let from = from_inclusive.number();
        let to = to_exclusive.number();

        if from >= to {
            return;
        }

        let start = self.entry_pos(from_inclusive);
        let end = self.entry_pos(to_exclusive);

        if start.word == end.word {
            let mask = Self::mask_range(start.bit, end.bit);
            if mask != 0 {
                self.write(start.word, |v| v | mask);
            }
            return;
        }

        self.write(start.word, |v| v | Self::mask_from(start.bit));

        for word_index in (start.word + 1)..end.word {
            self.write(word_index, |_| u64::MAX);
        }

        if end.bit > 0 {
            self.write(end.word, |v| v | Self::mask_until(end.bit));
        }
    }

    fn set_unchecked(&self, frame: Frame) {
        let pos = self.entry_pos(frame);
        self.write(pos.word, |v| v | (1u64 << pos.bit));
    }

    #[inline]
    fn is_in_range(&self, frame: Frame) -> bool {
        self.target_region.contains(frame.page_address())
    }

    /// Очистить бит в битовой карте (пометить как свободный)
    pub fn clear(&self, frame: Frame) {
        if !self.is_in_range(frame) {
            return;
        }

        let pos = self.entry_pos(frame);
        self.write(pos.word, |v| v & !(1u64 << pos.bit));
    }

    #[inline]
    fn entry_pos(&self, frame: Frame) -> EntryPos {
        let relative = frame.number().saturating_sub(self.base_frame.number());
        EntryPos::new(
            relative / Self::BITS_PER_ENTRY,
            relative % Self::BITS_PER_ENTRY,
        )
    }

    /// Находит первый свободный бит (0) в слове в пределах указанной маски.
    #[inline]
    fn first_free_bit(word: u64, allowed_mask: u64) -> Option<usize> {
        let free_bits = !word & allowed_mask;
        if free_bits == 0 {
            None
        } else {
            Some(free_bits.trailing_zeros() as usize)
        }
    }

    #[inline]
    fn try_allocate_in_word(&self, word_index: usize, allowed_mask: u64) -> Option<Frame> {
        if allowed_mask == 0 {
            return None;
        }

        let word_value = self.read(word_index);
        let bit_index = Self::first_free_bit(word_value, allowed_mask)?;
        Some(self.allocate_frame_at(word_index, bit_index))
    }

    /// Выделяет фрейм по индексу слова и индексу бита
    #[inline]
    fn allocate_frame_at(&self, word_index: usize, bit_index: usize) -> Frame {
        let frame_number = self.base_frame.number() + word_index * Self::BITS_PER_ENTRY + bit_index;
        let frame = Frame::new(frame_number);
        self.set_unchecked(frame);
        frame
    }

    /// Выделяет в памяти свободный фрейм, начиная поиск с offset
    pub fn alloc_from(&self, offset: Frame) -> Option<Frame> {
        let region_end_exclusive = Frame::from(self.target_region.end()).add(1);
        let start = self.entry_pos(offset);
        let region_end = self.entry_pos(region_end_exclusive);

        // Поиск от offset до конца региона
        if let Some(frame) = self.search_range(start, region_end) {
            return Some(frame);
        }

        // Если не нашли, ищем от начала региона до offset
        let region_begin = self.entry_pos(self.base_frame);
        self.search_range(region_begin, start)
    }

    /// Вспомогательный метод для поиска свободного фрейма в диапазоне слов
    fn search_range(&self, start: EntryPos, end: EntryPos) -> Option<Frame> {
        if !start.is_before(end) {
            return None;
        }

        if start.word == end.word {
            return self.try_allocate_in_word(start.word, Self::mask_range(start.bit, end.bit));
        }

        if let Some(frame) = self.try_allocate_in_word(start.word, Self::mask_from(start.bit)) {
            return Some(frame);
        }

        for word in (start.word + 1)..end.word {
            if let Some(frame) = self.try_allocate_in_word(word, u64::MAX) {
                return Some(frame);
            }
        }

        if end.bit > 0 {
            return self.try_allocate_in_word(end.word, Self::mask_until(end.bit));
        }

        None
    }

    #[cfg_attr(not(test), doc(hidden))]
    pub fn bitmap_address(&self) -> PageAlignedAddress {
        self.bitmap_address
    }

    #[cfg_attr(not(test), doc(hidden))]
    pub fn managed_region(&self) -> &MemoryRange<PageAlignedAddress> {
        &self.target_region
    }

    #[cfg_attr(not(test), doc(hidden))]
    pub fn is_allocated(&self, frame: Frame) -> bool {
        if !self.is_in_range(frame) {
            return false;
        }

        let pos = self.entry_pos(frame);
        let value = self.read(pos.word);
        (value & (1u64 << pos.bit)) != 0
    }

    #[inline]
    fn mask_lower(bits: usize) -> u64 {
        if bits == 0 {
            0
        } else if bits >= Self::BITS_PER_ENTRY {
            u64::MAX
        } else {
            (1u64 << bits) - 1
        }
    }

    #[inline]
    fn mask_from(bit: usize) -> u64 {
        if bit >= Self::BITS_PER_ENTRY {
            0
        } else {
            !Self::mask_lower(bit)
        }
    }

    #[inline]
    fn mask_until(bits: usize) -> u64 {
        Self::mask_lower(bits)
    }

    #[inline]
    fn mask_range(start: usize, end: usize) -> u64 {
        if start >= end {
            0
        } else {
            Self::mask_until(end) & Self::mask_from(start)
        }
    }
}
