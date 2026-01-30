//! Аллокатор памяти ядра
//!
//! Этот модуль предоставляет аллокатор кучи для ядра, использующий алгоритм
//! first-fit со свободным списком и автоматическим расширением кучи.

use crate::region_manager::RegionManager;
use crate::virtual_address::VirtualAddress;
use core::alloc::Layout;
use core::mem::size_of;
use core::ptr::NonNull;

/// Минимальный размер выделения
/// Должен вместить: указатель на заголовок + минимум полезных данных
const MIN_ALLOC_SIZE: usize = 16;

/// Выравнивание для всех выделений
const ALLOC_ALIGN: usize = 8;

/// Размер указателя на заголовок, хранимого перед пользовательскими данными
const HEADER_PTR_SIZE: usize = size_of::<*mut FreeBlock>();

/// Размер страницы для выравнивания при расширении
const PAGE_SIZE: usize = 4096;

/// Типы ошибок при сбое выделения памяти
#[derive(Debug, Clone)]
pub enum AllocationError {
    OutOfMemory,
    InvalidLayout,
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
    const fn from_size(size: usize) -> Self {
        FreeBlock { size, next: None }
    }

    /// Разделить этот блок, если он достаточно большой для запрошенного размера.
    /// Возвращает новый блок, созданный из разделения, если возможно.
    ///
    /// Схема памяти блока:
    /// ```text
    /// [FreeBlock header (size, next)][usable memory of size `self.size`]
    /// ```
    fn split(&mut self, requested_size: usize) -> Option<NonNull<FreeBlock>> {
        let aligned_size = align_up(requested_size, ALLOC_ALIGN);
        let block_header_size = size_of::<FreeBlock>();

        // Проверяем, можем ли разделить: нужно место для запрошенного размера + заголовок нового блока + минимальный размер
        let min_remaining = block_header_size + MIN_ALLOC_SIZE;

        if self.size < aligned_size + min_remaining {
            return None;
        }

        // Вычисляем позицию нового блока: после заголовка текущего + выделяемая память
        let new_block_offset = block_header_size + aligned_size;
        // Размер нового блока: оригинальный размер - выделенный размер - заголовок нового блока
        let new_block_size = self.size - aligned_size - block_header_size;

        // Создаем новый блок в вычисленной позиции
        let new_block_ptr = unsafe {
            let base_ptr = self as *mut FreeBlock as usize;
            let new_ptr = (base_ptr + new_block_offset) as *mut FreeBlock;

            // Инициализируем новый блок
            *new_ptr = FreeBlock::from_size(new_block_size);

            NonNull::new_unchecked(new_ptr)
        };

        // Обновляем размер этого блока до выделенной части
        self.size = aligned_size;

        Some(new_block_ptr)
    }
}

pub struct HeapAllocator<'a> {
    /// Голова списка свободных блоков
    free_list_head: Option<NonNull<FreeBlock>>,

    /// Текущий размер кучи в байтах
    current_size: usize,

    /// Следующая позиция для расширения кучи
    cursor: VirtualAddress,

    region_manager: &'a RegionManager,
}

impl<'a> HeapAllocator<'a> {
    pub fn new(region_manager: &'a RegionManager, start: VirtualAddress) -> Self {
        HeapAllocator {
            free_list_head: None,
            current_size: 0,
            cursor: start,
            region_manager,
        }
    }

    /// Увеличивает емкость кучи
    fn expand(&mut self, expand_size: usize) -> Result<(), AllocationError> {
        let aligned_size = align_up(expand_size, PAGE_SIZE);

        // Получаем подсказку от менеджера регионов - курсор перепрыгивает к ближайшей RAM области
        let addr = self
            .region_manager
            .next_free(self.cursor, aligned_size)
            .ok_or(AllocationError::OutOfMemory)?;

        // Полезный размер блока
        let block_size = aligned_size - size_of::<FreeBlock>();

        unsafe {
            // Располагаем метаданные по указателю начала блока
            let block_ptr = addr.as_ptr();
            *block_ptr = FreeBlock::from_size(block_size);

            self.add_to_free_list(NonNull::new_unchecked(block_ptr));
        }

        // Сдвигаем курсор на следующую позицию
        self.cursor = addr.offset(aligned_size);
        self.current_size += aligned_size;

        Ok(())
    }

    /// Найти и удалить подходящий блок из списка свободных
    fn find_free_block(&mut self, size: usize) -> Option<NonNull<FreeBlock>> {
        let mut current = self.free_list_head;
        let mut prev: Option<NonNull<FreeBlock>> = None;

        while let Some(block_ptr) = current {
            unsafe {
                let block = block_ptr.as_ptr();

                // Блок подходит по размеру
                if (*block).size >= size {
                    // Удаляем из списка свободных
                    if let Some(prev_ptr) = prev {
                        (*prev_ptr.as_ptr()).next = (*block).next;
                    } else {
                        self.free_list_head = (*block).next;
                    }

                    // Разделяем блок, если он значительно больше
                    if let Some(new_block_ptr) = (*block).split(size) {
                        self.add_to_free_list(new_block_ptr);
                    }

                    return Some(block_ptr);
                }

                prev = current;
                current = (*block).next;
            }
        }

        None
    }

    /// Добавить блок в список свободных
    fn add_to_free_list(&mut self, block_ptr: NonNull<FreeBlock>) {
        unsafe {
            let mut block = block_ptr.read();
            block.next = self.free_list_head;
            block_ptr.write(block);
            self.free_list_head = Some(block_ptr);
        }
    }

    /// Выделить память
    ///
    /// Схема памяти выделенного блока:
    /// ```text
    /// [FreeBlock header][padding][ptr to header][user data (aligned)]
    ///                            ^              ^
    ///                   HEADER_PTR_SIZE         возвращаемый указатель
    /// ```
    pub fn allocate(&mut self, layout: Layout) -> Result<NonNull<u8>, AllocationError> {
        if layout.size() == 0 {
            return Err(AllocationError::InvalidLayout);
        }

        // Учитываем запрошенное выравнивание (минимум ALLOC_ALIGN)
        let align = layout.align().max(ALLOC_ALIGN);
        let size = layout.size().max(MIN_ALLOC_SIZE);

        // Нужно место для: данных + указателя на заголовок + возможный padding для выравнивания
        // В худшем случае padding = align - 1
        let alloc_size = size + HEADER_PTR_SIZE + align - 1;

        // Сначала ищем в free list
        if let Some(block_ptr) = self.find_free_block(alloc_size) {
            return Ok(self.setup_allocated_block(block_ptr, align));
        }

        // Не нашли — расширяем
        let needed_size = alloc_size + size_of::<FreeBlock>();
        let expand_size = needed_size.max(PAGE_SIZE);

        self.expand(expand_size)?;

        // Теперь точно найдём
        if let Some(block_ptr) = self.find_free_block(alloc_size) {
            Ok(self.setup_allocated_block(block_ptr, align))
        } else {
            Err(AllocationError::OutOfMemory)
        }
    }

    /// Подготовить выделенный блок: записать указатель на заголовок и вернуть выровненный указатель
    fn setup_allocated_block(&self, block_ptr: NonNull<FreeBlock>, align: usize) -> NonNull<u8> {
        unsafe {
            // Начало области данных (после заголовка)
            let data_start = (block_ptr.as_ptr() as *mut u8).add(size_of::<FreeBlock>());

            // Вычисляем адрес для пользовательских данных с учётом выравнивания
            // Нужно оставить HEADER_PTR_SIZE байт перед данными для указателя на заголовок
            let min_user_addr = data_start as usize + HEADER_PTR_SIZE;
            let aligned_user_addr = align_up(min_user_addr, align);
            let user_ptr = aligned_user_addr as *mut u8;

            // Записываем указатель на заголовок непосредственно перед пользовательскими данными
            let header_ptr_location = (user_ptr as *mut *mut FreeBlock).sub(1);
            *header_ptr_location = block_ptr.as_ptr();

            NonNull::new_unchecked(user_ptr)
        }
    }

    /// Освободить память
    ///
    /// Указатель на заголовок блока хранится непосредственно перед пользовательскими данными.
    /// После освобождения указатель обнуляется для защиты от double-free.
    pub fn deallocate(&mut self, ptr: NonNull<u8>) {
        unsafe {
            // Читаем указатель на заголовок блока, хранящийся перед пользовательскими данными
            let header_ptr_location = (ptr.as_ptr() as *mut *mut FreeBlock).sub(1);
            let block_ptr = *header_ptr_location;

            if let Some(block) = NonNull::new(block_ptr) {
                // Обнуляем указатель на заголовок для защиты от double-free
                *header_ptr_location = core::ptr::null_mut();

                self.add_to_free_list(block);
            }
            // Если block_ptr уже null (повторный deallocate), ничего не делаем
        }

        // TODO: Реализовать слияние смежных свободных блоков
    }
}

const fn align_up(addr: usize, align: usize) -> usize {
    (addr + align - 1) & !(align - 1)
}
