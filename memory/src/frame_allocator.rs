use core::sync::atomic::{AtomicUsize, Ordering};

use collections::{LockCell, Vec};

use crate::{
    frame::Frame, frame_bitmap::FrameBitmap, memory_range::MemoryRange,
    physical_address::PageAlignedAddress,
};

/// Ошибки при работе с фреймами
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameError {
    /// Фрейм находится за пределами управляемого диапазона
    OutOfRange,
    /// Фрейм не был выделен
    NotAllocated,
}

/// Ошибки при резервировании фреймов.
#[derive(Debug, Clone)]
pub enum ReserveFrameError {
    /// Запрошенный диапазон выходит за пределы управляемых регионов.
    OutOfTargetBoundary {
        /// Начало запрошенного диапазона (включительно).
        from_inclusive: Frame,
        /// Конец запрошенного диапазона (эксклюзивно).
        to_exclusive: Frame,
    },
}

/// Трейт аллокатора фреймов физической памяти.
pub trait FrameAllocator {
    /// Резервирует область физической памяти начиная с указанного адреса.
    /// Помечает фреймы как занятые без проверки их текущего состояния.
    fn reserve_frames_exact(
        &self,
        from_inclusive: Frame,
        to_exclusive: Frame,
    ) -> Result<Frame, ReserveFrameError>;

    /// Выделяет свободный фрейм
    fn allocate_frame(&self) -> Option<Frame>;

    /// Выделяет до `max_count` смежных страниц.
    /// Возвращает (первый фрейм, количество выделенных).
    fn allocate_frames(&self, max_count: usize) -> Option<(Frame, usize)>;

    /// Освобождает фрейм
    fn deallocate_frame(&self, frame: Frame) -> Result<(), FrameError>;

    fn is_allocated(&self, frame: Frame) -> bool;
}

/// Максимальное количество регионов памяти.
pub const MAX_REGIONS: usize = 24;

/// Менеджер физической памяти.
///
/// Управляет выделением и освобождением фреймов физической памяти
/// в заданных регионах ОЗУ. Поддерживает несколько регионов.
pub struct PhysicalFrameAllocator<L: LockCell<FrameBitmap>> {
    /// Управляемые регионы оперативной памяти.
    regions: Vec<L, MAX_REGIONS>,

    /// Индекс текущего региона для поиска свободных фреймов.
    current_region_index: usize,

    /// Номер следующего фрейма для выделения.
    next_frame_hint: AtomicUsize,
}

impl<L: LockCell<FrameBitmap>> PhysicalFrameAllocator<L> {
    pub fn new<T>(memory: T) -> Self
    where
        T: Iterator<Item = MemoryRange<PageAlignedAddress>>,
    {
        let regions: Vec<L, MAX_REGIONS> = memory
            .map(|range| L::new(FrameBitmap::new(range)))
            .inspect(|region| {
                region.with_lock(|bitmap| {
                    // Резервируем нулевой фрейм, чтобы адрес 0x0 оставался маркером
                    // невалидного указателя
                    let start = bitmap.start();
                    if start.is_zero() {
                        bitmap.set_unchecked(Frame::from(start));
                    }
                });
            })
            .collect();

        let next_frame_hint = regions
            .iter()
            .next()
            .expect("Managed memory regions should not be empty")
            .with_lock(|bitmap| {
                let start = bitmap.start();
                if start.is_zero() {
                    start.next_aligned()
                } else {
                    start
                }
            });

        Self {
            regions,
            current_region_index: 0,
            next_frame_hint: AtomicUsize::new(Frame::from(next_frame_hint).number()),
        }
    }

    /// Находит регион, содержащий диапазон фреймов [from_inclusive, to_exclusive)
    fn find_containing_region(&self, from_inclusive: Frame, to_exclusive: Frame) -> Option<&L> {
        // Преобразуем эксклюзивную границу в инклюзивную
        let to_inclusive = if to_exclusive.number() > from_inclusive.number() {
            to_exclusive.sub(1)
        } else {
            from_inclusive
        };

        for region in &self.regions {
            let region_range = region.with_lock(|region| region.range());

            if region_range.contains(from_inclusive.page_address())
                && region_range.contains(to_inclusive.page_address())
            {
                return Some(region);
            }
        }

        None
    }

    fn try_alloc_in(bitmap: &mut FrameBitmap, current: Frame) -> Option<Frame> {
        if bitmap.remaining() == 0usize {
            // В этом участке нет свободных фреймов
            return None;
        }

        if let Some(frame) = bitmap.alloc_from(current) {
            // Нашли следующий фрейм с последней позиции
            Some(frame)
        } else {
            // Дошли до конца, пробуем найти с начала
            bitmap.alloc_from(Frame::from(bitmap.start()))
        }
    }

    /// Релоцирует внутренние указатели bitmap'ов в каждом регионе.
    ///
    /// # Safety
    ///
    /// Вызывать ровно один раз после включения MMU при линейном отображении
    /// физической памяти в higher-half с константным `offset`.
    pub unsafe fn relocate_inner_pointers_by_offset(&self, offset: usize) {
        for region in &self.regions {
            region.with_lock(|bitmap| {
                // SAFETY: Выполняется в post-MMU фазе до первого использования allocator'а.
                unsafe { bitmap.relocate_ptr_by_offset(offset) };
            });
        }
    }
}

impl<L: LockCell<FrameBitmap>> FrameAllocator for PhysicalFrameAllocator<L> {
    fn reserve_frames_exact(
        &self,
        from_inclusive: Frame,
        to_exclusive: Frame,
    ) -> Result<Frame, ReserveFrameError> {
        let target_region = self.find_containing_region(from_inclusive, to_exclusive);

        if let Some(region) = target_region {
            region.with_lock(|bitmap| bitmap.set_range_unchecked(from_inclusive, to_exclusive));

            Ok(from_inclusive)
        } else {
            Err(ReserveFrameError::OutOfTargetBoundary {
                from_inclusive,
                to_exclusive,
            })
        }
    }

    /// Выделяет один фрейм памяти
    fn allocate_frame(&self) -> Option<Frame> {
        let next_frame_hint = Frame::new(self.next_frame_hint.load(Ordering::Relaxed));

        // Начало поиска с подсказки next_frame_hint в текущем регионе
        let current_region_frame = self.regions[self.current_region_index]
            .with_lock(|current| Self::try_alloc_in(current, next_frame_hint));

        if let Some(frame) = current_region_frame {
            self.next_frame_hint
                .store(frame.add(1).number(), Ordering::Relaxed);

            return Some(frame);
        }

        // Ищем в других регионах, пропуская уже проверенный
        for (index, region) in self.regions.iter().enumerate() {
            if index == self.current_region_index {
                continue; // Пропускаем уже проверенный регион
            }

            let region_frame = region.with_lock(|region_bitmap| {
                let range = region_bitmap.range();
                Self::try_alloc_in(region_bitmap, Frame::from(range.start()))
            });

            if let Some(frame) = region_frame {
                self.next_frame_hint
                    .store(frame.add(1).number(), Ordering::Relaxed);

                return Some(frame);
            }
        }

        // Нет свободных фреймов
        None
    }

    fn allocate_frames(&self, max_count: usize) -> Option<(Frame, usize)> {
        for region in &self.regions {
            let result = region.with_lock(|bitmap| bitmap.alloc_contiguous(max_count));
            if result.is_some() {
                return result;
            }
        }
        None
    }

    fn deallocate_frame(&self, frame: Frame) -> Result<(), FrameError> {
        // Передача эксклюзивной границы frame.add(1) для одного фрейма
        let target_region = self.find_containing_region(frame, frame.add(1));

        if let Some(region) = target_region {
            let was_allocated = region.with_lock(|bitmap| bitmap.clear(frame));
            if was_allocated {
                Ok(())
            } else {
                Err(FrameError::NotAllocated)
            }
        } else {
            // Фрейм находится за пределами управляемого диапазона памяти
            Err(FrameError::OutOfRange)
        }
    }

    fn is_allocated(&self, frame: Frame) -> bool {
        let region = self.find_containing_region(frame, frame);

        if let Some(region) = region {
            region.with_lock(|bitmap| bitmap.is_allocated(frame))
        } else {
            false
        }
    }
}
