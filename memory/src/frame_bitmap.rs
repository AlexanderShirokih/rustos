use crate::memory_backend::{MemoryBackend, MemoryBackendExt};
use crate::memory_range::{AvailableRegions, MemoryRange};
use crate::physical::{Frame, PageAlignedAddress};

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
}

unsafe impl<'a, B: MemoryBackend> Send for FrameBitmap<'a, B> {}
unsafe impl<'a, B: MemoryBackend> Sync for FrameBitmap<'a, B> {}

#[derive(Debug)]
pub enum FrameBitmapError {
    TooManyRegions,
    UnableToAllocate,
}

impl<'a, B: MemoryBackend> FrameBitmap<'a, B> {
    // Количество байт необходимых для одной записи
    const ENTRY_SIZE: usize = size_of::<u64>();

    // Количество фреймов, помечаемых в одну запись (один бит на фрейм)
    const FRAMES_PER_ENTRY: usize = FrameBitmap::<'a, B>::ENTRY_SIZE * 8; // 64 фрейма на u64

    pub fn new(
        memory_backend: &'a B,
        target_region: &MemoryRange<PageAlignedAddress>,
        excluded_regions: &[MemoryRange<PageAlignedAddress>],
    ) -> Result<Self, FrameBitmapError> {
        let required_frames = Self::calc_required_frames_for_bitmap(target_region);

        let mut target_areas: heapless::Vec<MemoryRange<PageAlignedAddress>, 32> =
            heapless::Vec::new();

        // Добавляем начальный регион.
        target_areas
            .push(target_region.clone())
            .map_err(|_| FrameBitmapError::UnableToAllocate)?;

        // Строим список "чистых" областей, в которых можно найти место под FrameBitmap
        Self::find_suitable_regions(&mut target_areas, excluded_regions, required_frames)
            .map_err(|_| FrameBitmapError::TooManyRegions)?;

        let alloc_area = target_areas
            .first()
            .ok_or(FrameBitmapError::UnableToAllocate)?;

        let frame_bitmap = FrameBitmap {
            memory_backend,
            base_frame: Frame::from(target_region.start()),
            target_region: target_region.clone(),
            bitmap_address: alloc_area.start(),
        };

        // Очищаем физическую память для аллокатора (обнуляем всю битовую карту)
        frame_bitmap.clear_bitmap(required_frames);

        // Помечаем сам регион битовой карты как использованный
        frame_bitmap.set_range_unchecked(
            Frame::from(alloc_area.start()),
            Frame::from(
                alloc_area
                    .start()
                    .as_physical_address()
                    .add(required_frames * alloc_area.frame_size)
                    .align_down(alloc_area.frame_size),
            ),
        );

        for exclude in excluded_regions {
            frame_bitmap
                .set_range_unchecked(Frame::from(exclude.start()), Frame::from(exclude.end()))
        }

        Ok(frame_bitmap)
    }

    fn clear_bitmap(&self, required_frames: usize) {
        let bitmap_size_bytes = required_frames * PageAlignedAddress::alignment();
        let zero_buffer = [0u8; 4096]; // Буфер для записи нулей
        let mut offset = 0;
        while offset < bitmap_size_bytes {
            let chunk_size = core::cmp::min(zero_buffer.len(), bitmap_size_bytes - offset);
            self.memory_backend.write_bytes(
                self.bitmap_address.as_physical_address().add(offset),
                &zero_buffer[..chunk_size],
            );
            offset += chunk_size;
        }
    }

    fn calc_required_frames_for_bitmap(region: &MemoryRange<PageAlignedAddress>) -> usize {
        let bitmap_size = region.frame_count().div_ceil(Self::ENTRY_SIZE);
        bitmap_size.div_ceil(region.frame_size)
    }

    fn find_suitable_regions(
        targets_out: &mut heapless::Vec<MemoryRange<PageAlignedAddress>, 32>,
        excludes: &[MemoryRange<PageAlignedAddress>],
        required_frames: usize,
    ) -> Result<(), ()> {
        for exclude in excludes {
            let src = core::mem::take(targets_out);

            for area in src.iter() {
                // Вырезаем исключаемый регион
                let subtract_result = area.subtract(exclude);

                match subtract_result {
                    AvailableRegions::None => {}

                    AvailableRegions::One(updated_area) => {
                        if updated_area.frame_count() >= required_frames {
                            targets_out.push(updated_area).map_err(|_| ())?;
                        }
                    }

                    AvailableRegions::Two { left, right } => {
                        if left.frame_count() >= required_frames {
                            targets_out.push(left).map_err(|_| ())?;
                        }
                        if right.frame_count() >= required_frames {
                            targets_out.push(right).map_err(|_| ())?;
                        }
                    }
                }
            }
        }

        Ok(())
    }

    #[inline]
    fn write<F: Fn(u64) -> u64>(&self, offset: usize, update: F) {
        let address = self
            .bitmap_address
            .as_physical_address()
            .add(offset * Self::ENTRY_SIZE);
        let value = update(self.memory_backend.read::<u64>(address));
        self.memory_backend
            .write_bytes(address, &value.to_le_bytes());
    }

    #[inline]
    fn read(&self, offset: usize) -> u64 {
        let address = self
            .bitmap_address
            .as_physical_address()
            .add(offset * Self::ENTRY_SIZE);
        self.memory_backend.read::<u64>(address)
    }

    /// Помечает область фреймов как выделенную
    pub fn set_range_unchecked(&self, from_inclusive: Frame, to_exclusive: Frame) {
        let from = from_inclusive.number();
        let to = to_exclusive.number();

        if from >= to {
            return;
        }

        let (start_word, start_bit) = self.relative_frame_indexes(from_inclusive);
        let (end_word, end_bit) = self.relative_frame_indexes(to_exclusive.sub(1));

        if start_word == end_word {
            // Если начальный и конечный фреймы находятся в одном слове, нужно установить
            // только определенные биты в этом слове, а не все 64 бита.
            let mask = if end_bit == Self::FRAMES_PER_ENTRY - 1 {
                !((1u64 << start_bit) - 1)
            } else {
                ((1u64 << (end_bit + 1)) - 1) & !((1u64 << start_bit) - 1)
            };
            self.write(start_word, |v| v | mask);
        } else {
            // Первое частичное слово
            let first_mask = !((1u64 << start_bit) - 1);
            self.write(start_word, |v| v | first_mask);

            // Полные промежуточные слова
            for word_index in (start_word + 1)..end_word {
                self.write(word_index, |_| 0xFFFF_FFFF_FFFF_FFFF);
            }

            // Последнее частичное слово
            let last_mask = (1u64 << (end_bit + 1)) - 1;
            self.write(end_word, |v| v | last_mask);
        }
    }

    fn set_unchecked(&self, frame: Frame) {
        let (word_index, bit_index) = self.relative_frame_indexes(frame);
        self.write(word_index, |v| v | (1u64 << bit_index));
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

        let (word_index, bit_index) = self.relative_frame_indexes(frame);
        self.write(word_index, |v| v & !(1u64 << bit_index));
    }

    #[inline]
    fn relative_frame_indexes(&self, frame: Frame) -> (usize, usize) {
        let relative_frame_index = frame.number().saturating_sub(self.base_frame.number());
        let word_index = relative_frame_index / Self::FRAMES_PER_ENTRY;
        let bit_index = relative_frame_index % Self::FRAMES_PER_ENTRY;

        (word_index, bit_index)
    }

    /// Находит первый свободный бит (0) в слове. Возвращает индекс бита или None, если слово полностью занято.
    #[inline]
    fn find_free_bit_in_word(word: u64) -> Option<usize> {
        if word == 0xFFFF_FFFF_FFFF_FFFF {
            return None;
        }
        // Находим первый нулевой бит, инвертируя слово и используя trailing_zeros
        Some((!word).trailing_zeros() as usize)
    }

    /// Выделяет фрейм по индексу слова и индексу бита
    #[inline]
    fn allocate_frame_at(&self, word_index: usize, bit_index: usize) -> Frame {
        let frame_number =
            self.base_frame.number() + word_index * Self::FRAMES_PER_ENTRY + bit_index;
        let frame = Frame::new(frame_number);
        self.set_unchecked(frame);
        frame
    }

    /// Выделяет в памяти свободный фрейм, начиная поиск с offset
    pub fn alloc_from(&self, offset: Frame) -> Option<Frame> {
        let target_region_end_frame = Frame::from(self.target_region.end());

        let (start_word, start_bit) = self.relative_frame_indexes(offset);
        let (end_word, _) = self.relative_frame_indexes(target_region_end_frame);

        // Поиск от offset до конца региона
        if let Some(frame) = self.search_range(start_word, start_bit, end_word, 0) {
            return Some(frame);
        }

        // Если не нашли, ищем от начала региона до offset
        let (offset_word, offset_bit) = self.relative_frame_indexes(offset);
        self.search_range(0, 0, offset_word, offset_bit)
    }

    /// Вспомогательный метод для поиска свободного фрейма в диапазоне слов
    fn search_range(
        &self,
        start_word: usize,
        start_bit: usize,
        end_word: usize,
        end_bit: usize,
    ) -> Option<Frame> {
        if start_word > end_word || (start_word == end_word && start_bit >= end_bit) {
            return None;
        }

        // Обрабатываем первое слово (может быть частичным)
        let word_value = self.read(start_word);
        let masked_word = if start_word == end_word {
            // Все в одном слове - маскируем оба конца
            let mask = ((1u64 << end_bit) - 1) & !((1u64 << start_bit) - 1);
            word_value | !mask
        } else {
            // Маскируем начало
            word_value | ((1u64 << start_bit) - 1)
        };

        if let Some(bit_index) = Self::find_free_bit_in_word(masked_word) {
            return Some(self.allocate_frame_at(start_word, bit_index));
        }

        // Если все в одном слове и не нашли - выходим
        if start_word == end_word {
            return None;
        }

        // Обрабатываем полные промежуточные слова
        for word_index in (start_word + 1)..end_word {
            let word_value = self.read(word_index);
            if let Some(bit_index) = Self::find_free_bit_in_word(word_value) {
                return Some(self.allocate_frame_at(word_index, bit_index));
            }
        }

        // Обрабатываем последнее слово (может быть частичным)
        if end_bit > 0 {
            let word_value = self.read(end_word);
            let masked_word = word_value | !((1u64 << end_bit) - 1);
            if let Some(bit_index) = Self::find_free_bit_in_word(masked_word) {
                return Some(self.allocate_frame_at(end_word, bit_index));
            }
        }

        None
    }
}
