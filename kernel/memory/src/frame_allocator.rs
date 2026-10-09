#![allow(unsafe_code)]

use core::{
    num::NonZeroUsize,
    sync::atomic::{AtomicUsize, Ordering},
};

use io::Vec;
use kbase::LockCell;

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

/// Трейт аллокатора фреймов физической памяти. Методы принимают `&self`. Реализация
/// отвечает за синхронизацию доступа к своему состоянию.
pub trait FrameAllocator {
    /// Резервирует диапазон `[from_inclusive, to_exclusive)`. Помечает каждый фрейм диапазона
    /// занятым без проверки его прежнего состояния - вызов идемпотентен для уже занятых
    /// фреймов. Отказывает [`ReserveFrameError::OutOfTargetBoundary`], если диапазон не
    /// умещается целиком в одном управляемом регионе.
    fn reserve_frames_exact(
        &self,
        from_inclusive: Frame,
        to_exclusive: Frame,
    ) -> Result<Frame, ReserveFrameError>;

    /// Выделяет свободный фрейм из любого управляемого региона.
    fn allocate_frame(&self) -> Option<Frame>;

    /// Выделяет `count` смежных фреймов внутри одного управляемого региона и возвращает
    /// первый из них. Участок не продолжается в соседний регион, даже если тот примыкает
    /// вплотную. Возвращает `None`, если ни один регион не содержит свободного участка такой
    /// длины.
    fn allocate_frames(&self, count: NonZeroUsize) -> Option<Frame>;

    /// Освобождает выделенный фрейм. Повторный вызов на тот же фрейм отказывает
    /// [`FrameError::NotAllocated`] - операция не идемпотентна.
    fn deallocate_frame(&self, frame: Frame) -> Result<(), FrameError>;

    fn is_allocated(&self, frame: Frame) -> bool;
}

/// Счёт фреймов физической памяти под управлением аллокатора.
pub trait FrameCount {
    /// Число фреймов во всех управляемых регионах, занятых и свободных.
    fn managed_frames(&self) -> usize;

    /// Число свободных фреймов во всех регионах на момент вызова.
    fn free_frames(&self) -> usize;
}

/// Предельное число регионов, которое вмещает [`PhysicalFrameAllocator`]: `new` отказывает
/// [`FrameAllocatorInitError::TooManyRegions`] на большем числе.
pub const MAX_REGIONS: usize = 24;

/// Ошибки инициализации [`PhysicalFrameAllocator`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameAllocatorInitError {
    /// Не передано ни одного региона - аллокатору нечем управлять.
    NoRegions,
    /// Регионов больше, чем вмещает [`MAX_REGIONS`].
    TooManyRegions,
}

/// Менеджер физической памяти.
///
/// Управляет выделением и освобождением фреймов физической памяти в заданных регионах ОЗУ,
/// до [`MAX_REGIONS`] штук. Параметр `L` определяет стратегию блокировки битмапа каждого
/// региона.
pub struct PhysicalFrameAllocator<L: LockCell<FrameBitmap>> {
    regions: Vec<L, MAX_REGIONS>,
    next_frame_hint: AtomicUsize,
}

// SAFETY: alloc/dealloc-методы берут `&self` и сериализуют доступ к битмапу
// через `LockCell::with_lock`. Взаимное исключение обеспечивает стратегия `L`
// на своих условиях: `MutexCell` - безусловно, `NoLockCell` - в однопоточном
// окружении.
unsafe impl<L: LockCell<FrameBitmap>> Send for PhysicalFrameAllocator<L> {}
// SAFETY: см. комментарий к `Send`.
unsafe impl<L: LockCell<FrameBitmap>> Sync for PhysicalFrameAllocator<L> {}

impl<L: LockCell<FrameBitmap>> PhysicalFrameAllocator<L> {
    pub fn new<T>(memory: T) -> Result<Self, FrameAllocatorInitError>
    where
        T: Iterator<Item = MemoryRange<PageAlignedAddress>>,
    {
        let mut regions: Vec<L, MAX_REGIONS> = Vec::new();
        for range in memory {
            let region = L::new(FrameBitmap::new(range));
            region.with_lock(|bitmap| {
                // Резервируем нулевой фрейм, чтобы адрес 0x0 оставался маркером
                // невалидного указателя
                let start = bitmap.start();
                if start.is_zero() {
                    bitmap.set_unchecked(Frame::from(start));
                }
            });
            regions
                .push(region)
                .ok_or(FrameAllocatorInitError::TooManyRegions)?;
        }

        let next_frame_hint = regions
            .iter()
            .next()
            .ok_or(FrameAllocatorInitError::NoRegions)?
            .with_lock(|bitmap| {
                let start = bitmap.start();
                if start.is_zero() {
                    start.next_aligned()
                } else {
                    start
                }
            });

        Ok(Self {
            regions,
            next_frame_hint: AtomicUsize::new(Frame::from(next_frame_hint).number()),
        })
    }

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

        // `alloc_from` обходит регион кольцом от подсказки, поэтому её отказ
        // означает отсутствие свободного фрейма во всём регионе.
        bitmap.alloc_from(current)
    }

    /// Переносит регионы под стратегию блокировки `M`, потребляя исходный
    /// аллокатор. Смена стратегии допустима только на границе фазы, где
    /// прежний экземпляр перестаёт быть достижимым.
    pub fn into_locked<M: LockCell<FrameBitmap>>(self) -> PhysicalFrameAllocator<M> {
        PhysicalFrameAllocator {
            regions: self.regions.map_into(|region| M::new(region.into_inner())),
            next_frame_hint: self.next_frame_hint,
        }
    }

    /// Релоцирует внутренние указатели bitmap в каждом регионе.
    ///
    /// # Safety
    ///
    /// Вызывать ровно один раз после включения MMU при линейном отображении
    /// физической памяти в higher-half с константным `offset`.
    pub unsafe fn relocate_inner_pointers_by_offset(&self, offset: usize) {
        for region in &self.regions {
            region.with_lock(|bitmap| {
                // SAFETY: relocate_ptr_by_offset требует однократного вызова после MMU и
                // уже выполненного linear mapping в higher-half со смещением offset -
                // это гарантирует контракт вызывающей функции (см. `# Safety` выше);
                // цикл вызывает его один раз на регион.
                unsafe { bitmap.relocate_ptr_by_offset(offset) };
            });
        }
    }
}

impl<L: LockCell<FrameBitmap>> FrameCount for PhysicalFrameAllocator<L> {
    fn managed_frames(&self) -> usize {
        self.regions
            .iter()
            .map(|region| region.with_lock(|bitmap| bitmap.range().frame_count()))
            .sum()
    }

    fn free_frames(&self) -> usize {
        self.regions
            .iter()
            .map(|region| region.with_lock(|bitmap| bitmap.remaining()))
            .sum()
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

    fn allocate_frame(&self) -> Option<Frame> {
        let next_frame_hint = Frame::new(self.next_frame_hint.load(Ordering::Relaxed));

        // Первый регион пробуем с подсказки next_frame_hint, остальные - с их
        // начала. `alloc_from` сам клампит подсказку к границам региона.
        for (index, region) in self.regions.iter().enumerate() {
            let region_frame = region.with_lock(|bitmap| {
                let start = if index == 0 {
                    next_frame_hint
                } else {
                    Frame::from(bitmap.range().start())
                };
                Self::try_alloc_in(bitmap, start)
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

    fn allocate_frames(&self, count: NonZeroUsize) -> Option<Frame> {
        self.regions
            .iter()
            .find_map(|region| region.with_lock(|bitmap| bitmap.alloc_contiguous(count)))
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
