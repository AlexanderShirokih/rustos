use crate::frame::Frame;
use crate::memory_range::MemoryRange;
use crate::physical_address::PageAlignedAddress;
use alloc::boxed::Box;
use alloc::vec;
use core::mem::size_of;

/// Позиция бита в битовой карте
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct EntryPos {
    // Индекс 64-битного слова
    word: usize,

    // индекс бита в слове
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

/// Битовая карта для отслеживания статуса выделения фреймов
pub struct FrameBitmap {
    // Битовая карта для отслеживания занятых участков
    bitmap: Box<[u64]>,

    // Количество свободных фреймов
    free: usize,

    // Область памяти, которая управляется битовой картой
    target_region: MemoryRange<PageAlignedAddress>,

    // Фрейм начала [target_region]
    base_frame: Frame,
}

impl FrameBitmap {
    // Количество фреймов (битов), описываемых одной записью
    const BITS_PER_ENTRY: usize = size_of::<u64>() * 8;

    pub fn new(target_region: MemoryRange<PageAlignedAddress>) -> Self {
        // Считаем размер вектора - сколько понадобится для разметки региона памяти
        let entry_count = Self::calc_entry_count(&target_region);
        let bitmap = vec![0u64; entry_count].into_boxed_slice();

        FrameBitmap {
            bitmap,
            free: target_region.frame_count(),
            base_frame: Frame::from(target_region.start()),
            target_region,
        }
    }

    fn calc_entry_count(region: &MemoryRange<PageAlignedAddress>) -> usize {
        region.frame_count().div_ceil(Self::BITS_PER_ENTRY)
    }

    pub fn remaining(&self) -> usize {
        self.free
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

    pub const fn start(&self) -> PageAlignedAddress {
        self.target_region.start()
    }

    pub const fn range(&self) -> MemoryRange<PageAlignedAddress> {
        self.target_region
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

        let mut allocated_count = 0usize;

        if start.word == end.word {
            let mask = Self::mask_range(start.bit, end.bit);
            if mask != 0 {
                let old_value = self.read(start.word);
                // Считаем только новые биты (те, что были 0 и станут 1)
                let new_bits = mask & !old_value;
                allocated_count += new_bits.count_ones() as usize;
                self.write(start.word, |v| v | mask);
            }
            self.free = self.free.saturating_sub(allocated_count);
            return;
        }

        // Первое слово
        let first_mask = Self::mask_from(start.bit);
        let old_first = self.read(start.word);
        allocated_count += (first_mask & !old_first).count_ones() as usize;
        self.write(start.word, |v| v | first_mask);

        // Средние слова (полностью заполняем)
        for word_index in (start.word + 1)..end.word {
            let old_value = self.read(word_index);
            allocated_count += (!old_value).count_ones() as usize;
            self.write(word_index, |_| u64::MAX);
        }

        // Последнее слово
        if end.bit > 0 {
            let last_mask = Self::mask_until(end.bit);
            let old_last = self.read(end.word);
            allocated_count += (last_mask & !old_last).count_ones() as usize;
            self.write(end.word, |v| v | last_mask);
        }

        self.free = self.free.saturating_sub(allocated_count);
    }

    pub fn set_unchecked(&mut self, frame: Frame) {
        let pos = self.entry_pos(frame);
        let mask = 1u64 << pos.bit;
        let old_value = self.read(pos.word);
        if (old_value & mask) == 0 {
            self.write(pos.word, |val| val | mask);
            self.free = self.free.saturating_sub(1);
        }
    }

    #[inline]
    pub fn is_in_range(&self, frame: Frame) -> bool {
        self.target_region.contains(frame.page_address())
    }

    /// Очищает бит в битовой карте (пометить как свободный).
    /// Возвращает true, если фрейм был выделен и успешно освобождён.
    /// Возвращает false, если фрейм уже был свободен или вне диапазона.
    pub fn clear(&mut self, frame: Frame) -> bool {
        if !self.is_in_range(frame) {
            return false;
        }

        let pos = self.entry_pos(frame);
        let mask = 1u64 << pos.bit;
        let old_value = self.read(pos.word);

        // Проверяем, был ли фрейм выделен
        if (old_value & mask) == 0 {
            return false; // Фрейм уже был свободен
        }

        self.write(pos.word, |v| v & !mask);
        self.free += 1;
        true
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

        // Поиск от начала региона до offset при неудаче
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

    pub fn is_allocated(&self, frame: Frame) -> bool {
        if !self.is_in_range(frame) {
            return false;
        }

        let pos = self.entry_pos(frame);
        let value = self.read(pos.word);
        (value & (1u64 << pos.bit)) != 0
    }

    /// Выделяет до `max_count` смежных страниц.
    /// Возвращает (первый фрейм, количество выделенных).
    ///
    /// Алгоритм first-fit: находит первый свободный участок и выделяет
    /// максимально возможное количество смежных страниц (до max_count).
    pub fn alloc_contiguous(&mut self, max_count: usize) -> Option<(Frame, usize)> {
        if max_count == 0 || self.free == 0 {
            return None;
        }

        // Максимальный номер фрейма в регионе (эксклюзивная граница)
        let region_frame_count = self.target_region.frame_count();
        let max_frame_num = self.base_frame.number() + region_frame_count;

        // Ищем первый свободный бит
        let mut word_idx = 0;
        while word_idx < self.bitmap.len() && self.bitmap[word_idx] == u64::MAX {
            word_idx += 1;
        }

        if word_idx >= self.bitmap.len() {
            return None;
        }

        // Находим первый свободный бит в этом слове
        let first_bit = (!self.bitmap[word_idx]).trailing_zeros() as usize;
        let start_frame_num = self.base_frame.number() + word_idx * Self::BITS_PER_ENTRY + first_bit;

        // Проверяем, что первый свободный бит в пределах региона
        if start_frame_num >= max_frame_num {
            return None;
        }

        // Подсчёт последовательных свободных битов (до max_count) в пределах региона
        let mut count = 0;
        let mut w = word_idx;
        let mut b = first_bit;

        while count < max_count && w < self.bitmap.len() {
            let word = self.bitmap[w];
            while b < Self::BITS_PER_ENTRY && count < max_count {
                // Проверяем, не вышли ли за границу региона
                let current_frame_num = self.base_frame.number() + w * Self::BITS_PER_ENTRY + b;
                if current_frame_num >= max_frame_num {
                    // Достигли конца региона
                    break;
                }

                if (word & (1u64 << b)) != 0 {
                    // Бит занят — прерываем
                    break;
                }
                count += 1;
                b += 1;
            }

            // Проверяем причину выхода из внутреннего цикла
            let current_frame_num = self.base_frame.number() + w * Self::BITS_PER_ENTRY + b;
            if current_frame_num >= max_frame_num {
                break; // Достигли конца региона
            }
            if b < Self::BITS_PER_ENTRY && (self.bitmap[w] & (1u64 << b)) != 0 {
                break; // Встретили занятый бит
            }
            w += 1;
            b = 0;
        }

        if count == 0 {
            return None;
        }

        // Помечаем биты как занятые
        let start_frame = Frame::new(start_frame_num);
        let end_frame = Frame::new(start_frame_num + count);
        self.set_range_unchecked(start_frame, end_frame);

        Some((start_frame, count))
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
