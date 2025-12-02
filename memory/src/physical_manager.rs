use crate::frame_bitmap::FrameBitmap;
use crate::memory_range::MemoryRange;
use crate::physical::{Frame, PageAlignedAddress};
use core::sync::atomic::{AtomicUsize, Ordering};
use kernel::console::stdout;
use kernel::info;
use spin::Mutex;

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

    /// Возвращает управляемую область памяти
    fn get_managed_region(&self) -> MemoryRange<PageAlignedAddress>;
}

/// Менеджер физической памяти.
/// Управляет выделением и освобождением фреймов физической памяти в заданном регионе ОЗУ.
pub struct PhysicalMemoryManager {
    /// Управляемый регион оперативной памяти
    memory: MemoryRange<PageAlignedAddress>,

    /// Начальный фрейм области
    start_frame: Frame,

    /// Последний фрейм области
    end_frame: Frame,

    /// Следующий фрейм для выделения
    next_frame_hint: AtomicUsize,

    /// Битовая карта выделенных фреймов
    allocated_frames: Mutex<FrameBitmap>,
}

impl PhysicalMemoryManager {
    pub fn new(
        memory: &MemoryRange<PageAlignedAddress>,
        excluded_regions: impl Iterator<Item = MemoryRange<PageAlignedAddress>>,
    ) -> Self {
        info!(
            stdout(),
            "Using RAM region from 0x{:#} to 0x{:#}. Which is {}KB total",
            memory.start().as_usize(),
            memory.end().as_usize(),
            memory.size() / 1024
        );

        let mut frame_bitmap = FrameBitmap::new(memory);

        // Помечаем исключённые регионы как занятые
        for exclude in excluded_regions {
            frame_bitmap.set_range_unchecked(
                Frame::from(exclude.start()),
                Frame::from(exclude.end()).add(1),
            )
        }

        let start_frame = Frame::from(memory.start());
        let end_frame = Frame::from(memory.end());

        Self {
            allocated_frames: Mutex::new(frame_bitmap),
            memory: memory.clone(),
            start_frame,
            end_frame,
            next_frame_hint: AtomicUsize::new(start_frame.number()),
        }
    }
}

impl FrameAllocator for PhysicalMemoryManager {
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

        let mut bitmap = self.allocated_frames.lock();
        bitmap.set_range_unchecked(from_inclusive, to_exclusive);

        Ok(from_inclusive)
    }

    /// Выделяет один фрейм памяти
    fn allocate_frame(&self) -> Option<Frame> {
        // Начинаем с подсказки next_frame_hint
        let current = self.next_frame_hint.load(Ordering::Relaxed);
        let mut bitmap = self.allocated_frames.lock();

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
    }

    fn deallocate_frame(&self, frame: Frame) -> Result<(), FrameError> {
        let frame_address = frame.page_address();

        if self.memory.contains(frame_address) {
            let mut bitmap = self.allocated_frames.lock();
            bitmap.clear(frame);

            Ok(())
        } else {
            // Фрейм находится за пределами управляемого диапазона памяти
            Err(FrameError::OutOfRange)
        }
    }

    fn get_managed_region(&self) -> MemoryRange<PageAlignedAddress> {
        self.memory.clone()
    }
}
