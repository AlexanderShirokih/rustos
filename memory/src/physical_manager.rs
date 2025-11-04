use crate::frame_bitmap::{FrameBitmap, FrameBitmapError};
use crate::memory_backend::MemoryBackend;
use crate::memory_range::MemoryRange;
use crate::physical::{Frame, PageAlignedAddress};
use core::sync::atomic::{AtomicUsize, Ordering};
use kernel_core::console::console;
use kernel_core::info;
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

/// Ошибки при создании менеджера физической памяти
#[derive(Debug)]
pub enum ManagerError {
    /// Не удалось создать битовую карту
    BitmapCreationFailed(FrameBitmapError),
    /// Некорректный диапазон памяти
    InvalidRange,
}

/// Трейт аллокатора фреймов физической памяти.
///
/// Предоставляет интерфейс для выделения и освобождения фреймов физической памяти.
/// Реализации этого трейта должны быть потокобезопасными (Send + Sync).
pub trait FrameAllocator: Send + Sync {
    /// Возвращает размер физического фрейма в байтах
    fn frame_size(&self) -> usize;

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
///
/// Управляет выделением и освобождением фреймов физической памяти в заданном регионе ОЗУ.
/// Использует битовую карту для эффективного отслеживания состояния фреймов и атомарную
/// подсказку для оптимизации поиска следующего свободного фрейма.
///
/// # Параметры типа
///
/// * `'a` - время жизни ссылки на backend памяти
/// * `B` - тип backend'а для доступа к физической памяти
pub struct PhysicalMemoryManager<'a, B: MemoryBackend> {
    /// Управляемый регион оперативной памяти
    memory: MemoryRange<PageAlignedAddress>,

    /// Начальный фрейм области
    start_frame: Frame,

    /// Последний фрейм области
    end_frame: Frame,

    /// Следующий фрейм для выделения
    next_frame_hint: AtomicUsize,

    /// Битовая карта выделенных фреймов
    allocated_frames: Mutex<FrameBitmap<'a, B>>,
}

impl<'a, B: MemoryBackend> PhysicalMemoryManager<'a, B> {
    pub fn new(
        memory_backend: &'a B,
        memory: &MemoryRange<PageAlignedAddress>,
        excluded_regions: &[MemoryRange<PageAlignedAddress>],
    ) -> Result<Self, ManagerError> {
        if memory.start() > memory.end() {
            return Err(ManagerError::InvalidRange);
        }

        info!(
            console(),
            "Using RAM region from 0x{:x} to 0x{:x}. Which is {}KB total\r\n",
            memory.start().as_usize(),
            memory.end().as_usize(),
            memory.size() / 1024
        );

        let frame_bitmap = FrameBitmap::new(memory_backend, &memory, excluded_regions)
            .map_err(ManagerError::BitmapCreationFailed)?;

        let start_frame: Frame = Frame::from(memory.start());
        let end_frame: Frame = Frame::from(memory.end());
        let manager = PhysicalMemoryManager {
            allocated_frames: Mutex::new(frame_bitmap),
            memory: memory.clone(),
            start_frame,
            end_frame,
            next_frame_hint: AtomicUsize::new(start_frame.number()),
        };

        Ok(manager)
    }
}

impl<'a, B: MemoryBackend> FrameAllocator for PhysicalMemoryManager<'a, B> {
    /// Возвращает размер физического фрейма в байтах
    #[inline]
    fn frame_size(&self) -> usize {
        self.memory.frame_size
    }

    fn reserve_frames_exact(
        &self,
        from_inclusive: Frame,
        to_exclusive: Frame,
    ) -> Result<Frame, ReserveFrameError> {
        if from_inclusive > self.start_frame || to_exclusive < self.end_frame {
            let bitmap = self.allocated_frames.lock();

            bitmap.set_range_unchecked(from_inclusive, to_exclusive);

            Ok(from_inclusive)
        } else {
            // Требуемая область находится за пределами области памяти
            Err(ReserveFrameError::OutOfTargetBoundary {
                boundary: self.memory.clone(),
                from_inclusive,
                to_exclusive,
            })
        }
    }

    /// Выделяет один фрейм памяти
    #[inline]
    fn allocate_frame(&self) -> Option<Frame> {
        // Начинаем с подсказки next_frame_hint
        let current = self.next_frame_hint.load(Ordering::Relaxed);
        let bitmap = self.allocated_frames.lock();

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
            let bitmap = self.allocated_frames.lock();
            bitmap.clear(frame);

            Ok(())
        } else {
            // Фрейм находится за пределами управляемого диапазона памяти
            Err(FrameError::OutOfRange)
        }
    }
}
