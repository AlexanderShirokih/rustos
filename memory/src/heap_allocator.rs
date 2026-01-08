//! Аллокатор памяти ядра
//!
//! Этот модуль предоставляет аллокатор кучи для ядра, использующий алгоритм
//! first-fit со свободным списком и автоматическим расширением кучи.

use crate::aligned::Aligned;
use crate::memory_mapper::{MemoryMapper, MemoryMappingError};
use crate::memory_range::MemoryRange;
use crate::physical_address::PageAlignedAddress;
use crate::virtual_address::{PageAlignedVirtualAddress, VirtualAddress};
use core::alloc::Layout;
use core::mem::size_of;
use core::ptr::NonNull;
use core::sync::atomic::{AtomicUsize, Ordering};

/// Начальный размер кучи (1MB)
pub const HEAP_INITIAL_SIZE: usize = 1024 * 1024;

/// Минимальный размер выделения (16 байт)
const MIN_ALLOC_SIZE: usize = 16;

/// Выравнивание для всех выделений
const ALLOC_ALIGN: usize = 8;

/// Типы ошибок при сбое выделения памяти
#[derive(Debug, Clone)]
pub enum AllocationError {
    OutOfMemory,
    MappingFailed,
    InvalidLayout,
    AlignmentError,
}

/// Свободный блок в куче
#[derive(Clone, Copy, Debug)]
#[repr(C)]
struct FreeBlock {
    size: usize,
    next: Option<NonNull<FreeBlock>>,
}

impl FreeBlock {
    /// Создать новый свободный блок
    const fn new(size: usize) -> Self {
        FreeBlock { size, next: None }
    }

    /// Разделить этот блок, если он достаточно большой для запрошенного размера.
    /// Возвращает новый блок, созданный из разделения, если возможно
    fn split(&mut self, requested_size: usize) -> Option<NonNull<FreeBlock>> {
        let aligned_size = align_up(requested_size, ALLOC_ALIGN);
        let block_header_size = size_of::<FreeBlock>();

        // Проверяем, можем ли разделить: нужно место для запрошенного размера + заголовок нового блока + минимальный размер
        let min_remaining = block_header_size + MIN_ALLOC_SIZE;
        if self.size < aligned_size + min_remaining {
            return None;
        }

        // Вычисляем позицию и размер нового блока
        let new_block_offset = aligned_size;
        let new_block_size = self.size - aligned_size;

        // Создаем новый блок в вычисленной позиции
        let new_block_ptr = unsafe {
            let base_ptr = self as *mut FreeBlock as usize;
            let new_ptr = (base_ptr + new_block_offset) as *mut FreeBlock;

            // Инициализируем новый блок
            *new_ptr = FreeBlock::new(new_block_size);

            NonNull::new_unchecked(new_ptr)
        };

        // Обновляем размер этого блока до выделенной части
        self.size = aligned_size;

        Some(new_block_ptr)
    }
}

/// First-fit аллокатор кучи с автоматическим расширением
pub struct HeapAllocator<M: MemoryMapper> {
    /// Голова списка свободных блоков
    free_list: Option<NonNull<FreeBlock>>,

    /// Текущий размер кучи в байтах
    current_size: usize,

    /// Следующая позиция выделения для расширения кучи
    next_alloc_addr: AtomicUsize,

    mapper: M,

    heap_region: MemoryRange<PageAlignedAddress>,
}

impl<M: MemoryMapper> HeapAllocator<M> {
    pub fn new(mapper: M, heap_region: MemoryRange<PageAlignedAddress>) -> Self {
        HeapAllocator {
            free_list: None,
            current_size: 0,
            next_alloc_addr: AtomicUsize::new(heap_region.start().as_usize()),
            heap_region,
            mapper,
        }
    }

    /// Инициализировать аллокатор с начальным пространством кучи
    pub fn init(&mut self) -> Result<(), AllocationError> {
        self.expand_heap(HEAP_INITIAL_SIZE)
    }

    /// Расширить кучу путем выделения дополнительной памяти
    fn expand_heap(&mut self, additional_size: usize) -> Result<(), AllocationError> {
        let aligned_size = align_up(additional_size, PageAlignedVirtualAddress::ALIGNMENT);

        let mut start_addr =
            PageAlignedVirtualAddress::from_usize(self.next_alloc_addr.load(Ordering::Relaxed))
                .ok_or(AllocationError::AlignmentError)?;

        if (start_addr.as_usize() + aligned_size) > self.heap_region.end().as_usize() {
            return Err(AllocationError::OutOfMemory);
        }

        loop {
            // Отображаем физические фреймы в виртуальную область кучи
            let error = self.mapper.map_frames(&start_addr, aligned_size).err();

            match error {
                Some(e) => match e {
                    MemoryMappingError::AlreadyMapped => {
                        start_addr = start_addr.next_aligned();
                    }
                    _ => return Err(AllocationError::MappingFailed),
                },

                None => break,
            }
        }

        // Создаем новый свободный блок для расширенной области
        let block_addr: VirtualAddress = start_addr.into();
        let block_size = aligned_size - size_of::<FreeBlock>();
        let new_block = FreeBlock::new(block_size);

        // Записываем новый блок в память
        unsafe {
            block_addr.write(&new_block);
        }

        // Добавляем в список свободных блоков
        self.add_to_free_list(start_addr.as_usize());

        let virt: VirtualAddress = start_addr.into();
        let next_aligned_alloc_addr = PageAlignedVirtualAddress::new(virt.add(aligned_size))
            .ok_or(AllocationError::AlignmentError)?;

        self.current_size += aligned_size;
        self.next_alloc_addr
            .store(next_aligned_alloc_addr.as_usize(), Ordering::Relaxed);

        Ok(())
    }

    /// Добавить блок в список свободных
    fn add_to_free_list(&mut self, addr: usize) {
        let block_ptr = NonNull::new(addr as *mut FreeBlock).expect("Invalid address");
        let addr = VirtualAddress::new(addr);

        // Читаем блок, обновляем его указатель next и записываем обратно
        unsafe {
            let mut block: FreeBlock = core::ptr::read(addr.as_ptr());
            block.next = self.free_list;

            core::ptr::write(addr.as_ptr(), block);
        }

        // Обновляем голову списка свободных блоков
        self.free_list = Some(block_ptr);
    }

    /// Найти и удалить подходящий блок из списка свободных
    fn find_free_block(&mut self, size: usize) -> Option<NonNull<FreeBlock>> {
        let mut current = self.free_list;
        let mut prev: Option<NonNull<FreeBlock>> = None;

        while let Some(block_ptr) = current {
            unsafe {
                let block_ptr = block_ptr.as_ptr();
                let mut block: FreeBlock = core::ptr::read(block_ptr);

                // Проверяем, достаточно ли велик этот блок
                if block.size >= size {
                    // Удаляем из списка свободных
                    if let Some(prev_ptr) = prev {
                        let mut prev_block: FreeBlock = core::ptr::read(prev_ptr.as_ptr());
                        prev_block.next = block.next;

                        core::ptr::write(block_ptr, prev_block);
                    } else {
                        self.free_list = block.next;
                    }

                    // Разделяем блок, если он значительно больше, чем нужно
                    if let Some(new_block_ptr) = block.split(size) {
                        let new_addr = new_block_ptr.as_ptr() as usize;
                        self.add_to_free_list(new_addr);
                    }

                    // Записываем обратно выделенный блок
                    core::ptr::write(block_ptr, block);

                    return NonNull::new(block_ptr);
                }

                // Переходим к следующему блоку
                prev = current;
                current = block.next;
            }
        }

        None
    }

    pub fn allocate(&mut self, layout: Layout) -> Result<NonNull<u8>, AllocationError> {
        if layout.size() == 0 {
            return Err(AllocationError::InvalidLayout);
        }

        let size = layout.size().max(MIN_ALLOC_SIZE);

        // Пытаемся найти подходящий блок
        if let Some(block_ptr) = self.find_free_block(size) {
            let ptr = block_ptr.as_ptr() as *mut u8;
            return Ok(NonNull::new(ptr).expect("Block pointer should not be null"));
        }

        // Подходящий блок не найден, расширяем кучу
        let needed_size = size + size_of::<FreeBlock>();
        let expand_size = needed_size.max(PageAlignedVirtualAddress::ALIGNMENT);

        self.expand_heap(expand_size)?;

        // Пытаемся выделить снова после расширения
        if let Some(block_ptr) = self.find_free_block(size) {
            let ptr = block_ptr.as_ptr() as *mut u8;
            Ok(NonNull::new(ptr).expect("Block pointer should not be null"))
        } else {
            Err(AllocationError::OutOfMemory)
        }
    }

    /// Освободить память
    pub fn deallocate(&mut self, ptr: NonNull<u8>) {
        let addr = ptr.as_ptr() as usize;
        self.add_to_free_list(addr);

        // TODO: Реализовать слияние смежных свободных блоков для уменьшения фрагментации
    }
}

/// Вспомогательная функция для выравнивания вверх до указанного выравнивания
const fn align_up(addr: usize, align: usize) -> usize {
    (addr + align - 1) & !(align - 1)
}
