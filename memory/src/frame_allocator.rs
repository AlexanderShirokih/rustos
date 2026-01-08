use crate::frame::Frame;
use crate::frame_bitmap::FrameBitmap;
use crate::memory_range::MemoryRange;
use crate::physical_address::PageAlignedAddress;
use collections::{LockCell, MutexCell};
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
        boundary: MemoryRange<PageAlignedAddress>,
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

/// Менеджер физической памяти.
/// Управляет выделением и освобождением фреймов физической памяти в заданном регионе ОЗУ.
pub struct PhysicalFrameAllocator<L: LockCell<FrameBitmap>> {
    /// Управляемый регион оперативной памяти
    memory: MemoryRange<PageAlignedAddress>,

    /// Начальный фрейм области
    start_frame: Frame,

    /// Последний фрейм области
    end_frame: Frame,

    /// Следующий фрейм для выделения
    next_frame_hint: AtomicUsize,

    /// Битовая карта выделенных фреймов
    allocated_frames: L,
}

impl<L: LockCell<FrameBitmap>> PhysicalFrameAllocator<L> {
    pub fn new<T>(memory: &MemoryRange<PageAlignedAddress>, excluded_regions: T) -> Self
    where
        T: IntoIterator<Item = MemoryRange<PageAlignedAddress>>,
    {
        let mut frame_bitmap = FrameBitmap::new(memory);

        // Помечаем исключённые регионы как занятые
        let iter = excluded_regions.into_iter();
        iter.for_each(|exclude| {
            frame_bitmap.set_range_unchecked(
                Frame::from(exclude.start()),
                Frame::from(exclude.end()).add(1),
            )
        });

        let start_frame = Frame::from(memory.start());
        let end_frame = Frame::from(memory.end());

        Self {
            allocated_frames: L::new(frame_bitmap),
            memory: memory.clone(),
            start_frame,
            end_frame,
            next_frame_hint: AtomicUsize::new(start_frame.number()),
        }
    }

    pub fn heap_range(&self) -> MemoryRange<PageAlignedAddress> {
        MemoryRange::new(
            self.start_frame.page_address(),
            self.end_frame.page_address(),
        )
    }

    pub fn into_mutex(self) -> PhysicalFrameAllocator<MutexCell<FrameBitmap>> {
        PhysicalFrameAllocator::<MutexCell<FrameBitmap>> {
            memory: self.memory,
            start_frame: self.start_frame,
            end_frame: self.end_frame,
            next_frame_hint: self.next_frame_hint,
            allocated_frames: MutexCell::new(self.allocated_frames.into_inner()),
        }
    }
}

impl<L: LockCell<FrameBitmap>> FrameAllocator for PhysicalFrameAllocator<L> {
    fn reserve_frames_exact(
        &self,
        from_inclusive: Frame,
        to_exclusive: Frame,
    ) -> Result<Frame, ReserveFrameError> {
        let region_end_exclusive = self.end_frame.add(1);
        if from_inclusive < self.start_frame
            || to_exclusive > region_end_exclusive
            || from_inclusive >= to_exclusive
        {
            return Err(ReserveFrameError::OutOfTargetBoundary {
                boundary: self.memory.clone(),
                from_inclusive,
                to_exclusive,
            });
        }

        self.allocated_frames
            .with_lock(|bitmap| bitmap.set_range_unchecked(from_inclusive, to_exclusive));

        Ok(from_inclusive)
    }

    /// Выделяет один фрейм памяти
    fn allocate_frame(&self) -> Option<Frame> {
        // Начинаем с подсказки next_frame_hint
        let current = self.next_frame_hint.load(Ordering::Relaxed);

        self.allocated_frames.with_lock(|bitmap| {
            if let Some(frame) = bitmap.alloc_from(Frame::new(current)) {
                let next_frame_number = frame.add(1);

                let next_frame_hint = if next_frame_number.number() > self.end_frame.number() {
                    self.start_frame
                } else {
                    next_frame_number
                };

                self.next_frame_hint
                    .store(next_frame_hint.number(), Ordering::Relaxed);

                Some(frame)
            } else {
                // Нет свободных фреймов
                None
            }
        })
    }

    fn deallocate_frame(&self, frame: Frame) -> Result<(), FrameError> {
        let frame_address = frame.page_address();

        if self.memory.contains(frame_address) {
            self.allocated_frames
                .with_lock(|bitmap| bitmap.clear(frame));
            Ok(())
        } else {
            // Фрейм находится за пределами управляемого диапазона памяти
            Err(FrameError::OutOfRange)
        }
    }
}
