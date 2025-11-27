use crate::memory_range::MemoryRange;
use crate::physical::{Frame, PageAlignedAddress};
use alloc::boxed::Box;
use alloc::vec;
use core::mem::size_of;

/// Битовая карта для отслеживания статуса выделения фреймов
pub struct FrameBitmap {
    // Битовая карта, выделенная через глобальный аллокатор
    bitmap: Box<[u64]>,

    // Область памяти, которая управляется битовой картой
    target_region: MemoryRange<PageAlignedAddress>,

    // Фрейм начала [target_region]
    base_frame: Frame,
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

impl FrameBitmap {
    // Количество байт, необходимых для одной записи в битовой карте
    const ENTRY_BYTES: usize = size_of::<u64>();

    // Количество фреймов (битов), описываемых одной записью
    const BITS_PER_ENTRY: usize = Self::ENTRY_BYTES * 8;

    /// Создаёт новый FrameBitmap, выделяя память для битовой карты через глобальный аллокатор
    pub fn new(target_region: &MemoryRange<PageAlignedAddress>) -> Self {
        let entry_count = Self::calc_entry_count(target_region);

        let bitmap = vec![0u64; entry_count].into_boxed_slice();

        FrameBitmap {
            bitmap,
            base_frame: Frame::from(target_region.start()),
            target_region: target_region.clone(),
        }
    }

    /// Вычисляет количество u64 записей, необходимых для битовой карты
    fn calc_entry_count(region: &MemoryRange<PageAlignedAddress>) -> usize {
        region.frame_count().div_ceil(Self::BITS_PER_ENTRY)
    }

    #[inline]
    fn write<F: Fn(u64) -> u64>(&mut self, index: usize, update: F) {
        if index < self.bitmap.len() {
            self.bitmap[index] = update(self.bitmap[index]);
        }
    }

    #[inline]
    fn read(&self, offset: usize) -> u64 {
        if offset < self.bitmap.len() {
            self.bitmap[offset]
        } else {
            u64::MAX // За пределами - считаем все биты занятыми
        }
    }

    pub fn get_managed_region(&self) -> &MemoryRange<PageAlignedAddress> {
        &self.target_region
    }

    /// Помечает область фреймов как выделенную
    pub fn set_range_unchecked(&mut self, from_inclusive: Frame, to_exclusive: Frame) {
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

    fn set_unchecked(&mut self, frame: Frame) {
        let pos = self.entry_pos(frame);
        self.write(pos.word, |v| v | (1u64 << pos.bit));
    }

    #[inline]
    fn is_in_range(&self, frame: Frame) -> bool {
        self.target_region.contains(frame.page_address())
    }

    /// Очистить бит в битовой карте (пометить как свободный)
    pub fn clear(&mut self, frame: Frame) {
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
    fn try_allocate_in_word(&mut self, word_index: usize, allowed_mask: u64) -> Option<Frame> {
        if allowed_mask == 0 {
            return None;
        }

        let word_value = self.read(word_index);
        let bit_index = Self::first_free_bit(word_value, allowed_mask)?;
        Some(self.allocate_frame_at(word_index, bit_index))
    }

    /// Выделяет фрейм по индексу слова и индексу бита
    #[inline]
    fn allocate_frame_at(&mut self, word_index: usize, bit_index: usize) -> Frame {
        let frame_number = self.base_frame.number() + word_index * Self::BITS_PER_ENTRY + bit_index;
        let frame = Frame::new(frame_number);
        self.set_unchecked(frame);
        frame
    }

    /// Выделяет в памяти свободный фрейм, начиная поиск с offset
    pub fn alloc_from(&mut self, offset: Frame) -> Option<Frame> {
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
    fn search_range(&mut self, start: EntryPos, end: EntryPos) -> Option<Frame> {
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

    fn mask_lower(bits: usize) -> u64 {
        if bits == 0 {
            0
        } else if bits >= Self::BITS_PER_ENTRY {
            u64::MAX
        } else {
            (1u64 << bits) - 1
        }
    }

    fn mask_from(bit: usize) -> u64 {
        if bit >= Self::BITS_PER_ENTRY {
            0
        } else {
            !Self::mask_lower(bit)
        }
    }

    fn mask_until(bits: usize) -> u64 {
        Self::mask_lower(bits)
    }

    fn mask_range(start: usize, end: usize) -> u64 {
        if start >= end {
            0
        } else {
            Self::mask_until(end) & Self::mask_from(start)
        }
    }
}
