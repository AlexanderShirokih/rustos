//! Аллокатор виртуальных адресов для MMIO-окон.
//!
//! Управляет только диапазонами VA, без backing RAM. Метаданные
//! хранятся в обычной памяти (куча/статик), а не внутри VA-окна.

use crate::align::align_up_checked;
use crate::aligned::Aligned;
use crate::virtual_address::PageAlignedVirtualAddress;
use collections::interval_set::IntervalSet;
use core::alloc::Layout;
use core::fmt::{Display, Formatter};

/// Ошибки аллокации виртуального адресного пространства для MMIO.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MmioVmAllocError {
    /// Некорректный layout (нулевой размер или переполнение).
    InvalidLayout,
    /// Недостаточно свободного адресного пространства.
    OutOfSpace,
    /// Нехватка ёмкости для хранения свободных интервалов.
    CapacityExceeded,
}

impl Display for MmioVmAllocError {
    fn fmt(&self, f: &mut Formatter<'_>) -> core::fmt::Result {
        match self {
            MmioVmAllocError::InvalidLayout => f.write_str("Invalid layout"),
            MmioVmAllocError::OutOfSpace => f.write_str("Out of virtual address space"),
            MmioVmAllocError::CapacityExceeded => f.write_str("Interval set capacity exceeded"),
        }
    }
}

/// Аллокатор диапазонов VA для MMIO.
pub struct MmioVmAllocator {
    free_ranges: IntervalSet<PageAlignedVirtualAddress>,
}

impl MmioVmAllocator {
    /// Создаёт пустой аллокатор.
    pub fn empty() -> Self {
        Self {
            free_ranges: IntervalSet::new(),
        }
    }

    /// Добавляет свободный диапазон [start, end).
    pub fn add_range(
        &mut self,
        start: PageAlignedVirtualAddress,
        end: PageAlignedVirtualAddress,
    ) -> Result<(), MmioVmAllocError> {
        self.free_ranges
            .add(start, end)
            .ok_or(MmioVmAllocError::CapacityExceeded)
    }

    /// Выделяет виртуальный адрес из пула свободных адресов.
    /// Возвращаемый адрес не будет привязан к Heap области
    pub fn allocate(
        &mut self,
        layout: Layout,
    ) -> Result<PageAlignedVirtualAddress, MmioVmAllocError> {
        if layout.size() == 0 {
            return Err(MmioVmAllocError::InvalidLayout);
        }

        let page_size = PageAlignedVirtualAddress::ALIGNMENT;
        let align = layout.align().max(page_size);
        if !align.is_power_of_two() {
            return Err(MmioVmAllocError::InvalidLayout);
        }

        let size =
            align_up_checked(layout.size(), page_size).ok_or(MmioVmAllocError::InvalidLayout)?;

        for idx in 0..self.free_ranges.len() {
            let range = *self.free_ranges.get(idx).expect("index in bounds");
            let range_start = range.start.as_usize();
            let range_end = range.end.as_usize();

            let aligned_start =
                align_up_checked(range_start, align).ok_or(MmioVmAllocError::InvalidLayout)?;
            let aligned_end = aligned_start
                .checked_add(size)
                .ok_or(MmioVmAllocError::InvalidLayout)?;

            if aligned_end <= range_end {
                let alloc_start = PageAlignedVirtualAddress::from_usize(aligned_start)
                    .ok_or(MmioVmAllocError::InvalidLayout)?;
                let alloc_end = PageAlignedVirtualAddress::from_usize(aligned_end)
                    .ok_or(MmioVmAllocError::InvalidLayout)?;

                self.free_ranges
                    .remove(alloc_start, alloc_end)
                    .ok_or(MmioVmAllocError::CapacityExceeded)?;

                return Ok(alloc_start);
            }
        }

        Err(MmioVmAllocError::OutOfSpace)
    }

    /// Освобождает память
    pub fn free(
        &mut self,
        start: PageAlignedVirtualAddress,
        size: usize,
    ) -> Result<(), MmioVmAllocError> {
        if size == 0 {
            return Err(MmioVmAllocError::InvalidLayout);
        }

        let page_size = PageAlignedVirtualAddress::ALIGNMENT;
        let size = align_up_checked(size, page_size).ok_or(MmioVmAllocError::InvalidLayout)?;
        let end = start
            .as_usize()
            .checked_add(size)
            .ok_or(MmioVmAllocError::InvalidLayout)?;
        let end =
            PageAlignedVirtualAddress::from_usize(end).ok_or(MmioVmAllocError::InvalidLayout)?;

        self.free_ranges
            .add(start, end)
            .ok_or(MmioVmAllocError::CapacityExceeded)
    }

    /// Возвращает текущие свободные диапазоны
    pub fn free_ranges(&self) -> &IntervalSet<PageAlignedVirtualAddress> {
        &self.free_ranges
    }
}
