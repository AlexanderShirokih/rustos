use core::{alloc::Layout, fmt::Formatter, ptr::NonNull};

use crate::{
    memory_range::MemoryRange,
    physical_address::{PageAlignedAddress, PhysicalAddress},
};

/// Простой bump-аллокатор для непрерывного диапазона памяти.
///
/// Выделяет память последовательно, увеличивая смещение. Освобождение
/// отдельных блоков не поддерживается - память освобождается только целиком.
pub struct BumpAllocator {
    /// Начальный адрес управляемого региона.
    start: usize,
    /// Конечный адрес управляемого региона (эксклюзивный).
    end: usize,
    /// Текущее смещение от начала (следующий свободный байт).
    offset: usize,
}

impl BumpAllocator {
    pub const fn new(from: PageAlignedAddress, to: PageAlignedAddress) -> Self {
        Self {
            start: from.as_usize(),
            end: to.as_usize(),
            offset: 0,
        }
    }

    const fn remaining(&self) -> usize {
        self.end - self.start - self.offset
    }

    /// Возвращает диапазон фактически использованной памяти [start, start + offset)
    pub fn used(&self) -> MemoryRange<PhysicalAddress> {
        MemoryRange::new(
            PhysicalAddress::new(self.start),
            PhysicalAddress::new(self.start).add(self.offset),
        )
    }

    pub fn allocate(&mut self, layout: Layout) -> Result<NonNull<u8>, BumpAllocError> {
        let align = layout.align();
        let size = layout.size();
        let base = self.start;
        let current = base + self.offset;
        let aligned = current.div_ceil(align) * align;

        let new_offset = (aligned - base)
            .checked_add(size)
            .ok_or(BumpAllocError::AddressOverflow)?;

        let end_addr = base
            .checked_add(new_offset)
            .ok_or(BumpAllocError::AddressOverflow)?;
        if end_addr > self.end {
            return Err(BumpAllocError::OutOfMemory {
                required_size: size,
                available_size: self.remaining(),
            });
        }

        self.offset = new_offset;

        unsafe { Ok(NonNull::new_unchecked(aligned as *mut u8)) }
    }
}

/// Ошибки при выделении памяти через bump-аллокатор.
pub enum BumpAllocError {
    /// Переполнение адреса при вычислении нового смещения.
    AddressOverflow,

    /// Недостаточно памяти для выделения.
    OutOfMemory {
        /// Запрошенный размер в байтах.
        required_size: usize,
        /// Доступный размер в байтах.
        available_size: usize,
    },
}

impl core::fmt::Display for BumpAllocError {
    fn fmt(&self, f: &mut Formatter<'_>) -> core::fmt::Result {
        match self {
            BumpAllocError::AddressOverflow => write!(f, "Address overflow"),

            BumpAllocError::OutOfMemory {
                required_size,
                available_size,
            } => {
                write!(
                    f,
                    "Out of memory: required {required_size} bytes, available {available_size} bytes"
                )
            }
        }
    }
}
