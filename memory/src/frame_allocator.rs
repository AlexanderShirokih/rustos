use crate::frame::Frame;
use crate::frame_bitmap::FrameBitmap;
use crate::memory_range::MemoryRange;
use crate::physical_address::PageAlignedAddress;
use collections::{LockCell, MutexCell, Vec};
use core::sync::atomic::{AtomicUsize, Ordering};

/// Ошибки при работе с фреймами
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameError {
    /// Фрейм находится за пределами управляемого диапазона
    OutOfRange,
    /// Фрейм не был выделен
    NotAllocated,
}

#[derive(Debug, Clone)]
pub enum ReserveFrameError {
    OutOfTargetBoundary {
        from_inclusive: Frame,
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

    /// Выделить свободный фрейм
    fn allocate_frame(&self) -> Option<Frame>;

    /// Освободить фрейм
    fn deallocate_frame(&self, frame: Frame) -> Result<(), FrameError>;
}

pub const MAX_REGIONS: usize = 24;

/// Менеджер физической памяти.
/// Управляет выделением и освобождением фреймов физической памяти в заданном регионе ОЗУ.
pub struct PhysicalFrameAllocator<L: LockCell<FrameBitmap>> {
    /// Управляемый регион оперативной памяти
    regions: Vec<L, MAX_REGIONS>,

    current_region_index: usize,

    /// Следующий фрейм для выделения
    next_frame_hint: AtomicUsize,
}

impl<L: LockCell<FrameBitmap>> PhysicalFrameAllocator<L> {
    pub fn new<T>(memory: T) -> Self
    where
        T: Iterator<Item = MemoryRange<PageAlignedAddress>>,
    {
        let regions: Vec<L, MAX_REGIONS> = memory
            .map(|range| L::new(FrameBitmap::new(range)))
            .collect();

        let next_frame_hint = regions
            .iter()
            .next()
            .expect("Managed memory regions should not be empty")
            .with_lock(|bitmap| bitmap.start());

        Self {
            regions,
            current_region_index: 0,
            next_frame_hint: AtomicUsize::new(Frame::from(next_frame_hint).number()),
        }
    }

    pub fn into_mutex(self) -> PhysicalFrameAllocator<MutexCell<FrameBitmap>> {
        PhysicalFrameAllocator::<MutexCell<FrameBitmap>> {
            regions: self
                .regions
                .into_iter()
                .map(|region| MutexCell::new(region.into_inner()))
                .collect(),
            current_region_index: self.current_region_index,
            next_frame_hint: self.next_frame_hint,
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

        // Начинаем с подсказки next_frame_hint в текущем регионе
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

    fn deallocate_frame(&self, frame: Frame) -> Result<(), FrameError> {
        // Передаём эксклюзивную границу frame.add(1) для одного фрейма
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
}
