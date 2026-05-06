//! Аллокатор памяти ядра
//!
//! Модуль предоставляет аллокатор кучи для ядра с алгоритмом
//! first-fit и автоматическим расширением.
//!
//! Работает с pre-mapped RAM: физическая память замаплена линейно
//! (VA = higher_half_base + PA).

#![allow(unsafe_code)]

use core::{alloc::Layout, mem::size_of, ptr::NonNull};

use crate::{
    align::align_up, frame_allocator::FrameAllocator, virtual_address::PageAlignedVirtualAddress,
};

/// Минимальный размер выделения
/// Должен вместить: указатель на заголовок + минимум полезных данных
const MIN_ALLOC_SIZE: usize = 16;

/// Выравнивание для всех выделений
const ALLOC_ALIGN: usize = 8;

/// Размер указателя на заголовок, хранимого перед пользовательскими данными
const HEADER_PTR_SIZE: usize = size_of::<*mut FreeBlock>();

/// Размер страницы для выравнивания при расширении
const PAGE_SIZE: usize = 4096;

/// Ошибки при выделении памяти в куче.
#[derive(Debug, Clone)]
pub enum AllocationError {
    /// Недостаточно памяти для выделения.
    OutOfMemory,
    /// Некорректный layout (например, нулевой размер).
    InvalidLayout,
}

/// Свободный блок в куче.
#[derive(Clone, Copy, Debug)]
#[repr(C)]
struct FreeBlock {
    /// Размер полезной области блока в байтах (без заголовка).
    size: usize,
    /// Указатель на следующий свободный блок.
    next: Option<NonNull<FreeBlock>>,
}

impl FreeBlock {
    /// Создает новый свободный блок
    const fn from_size(size: usize) -> Self {
        FreeBlock { size, next: None }
    }

    /// Разделяет блок при достаточном размере.
    /// Возвращает новый блок, если разделение возможно.
    ///
    /// Схема памяти:
    /// ```text
    /// [FreeBlock header][usable memory]
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
        // SAFETY: `self` указывает на валидный FreeBlock в куче размера `self.size + size_of::<FreeBlock>()`;
        // `new_block_offset = block_header_size + aligned_size` строго меньше этого размера
        // (проверено `self.size >= aligned_size + min_remaining`), значит итоговый адрес лежит
        // внутри той же выделенной области и подходит для записи `FreeBlock`.
        let new_block_ptr = unsafe {
            let base_ptr = core::ptr::from_mut::<FreeBlock>(self) as usize;
            let new_ptr = (base_ptr + new_block_offset) as *mut FreeBlock;

            *new_ptr = FreeBlock::from_size(new_block_size);

            NonNull::new_unchecked(new_ptr)
        };

        // Обновляем размер этого блока до выделенной части
        self.size = aligned_size;

        Some(new_block_ptr)
    }
}

/// Аллокатор кучи ядра на основе free list
pub struct HeapAllocator {
    /// Аллокатор физических фреймов
    frame_allocator: &'static dyn FrameAllocator,

    /// База higher half для преобразования PA -> VA
    higher_half_base: usize,

    /// Голова списка свободных блоков
    free_list_head: Option<NonNull<FreeBlock>>,

    /// Текущий размер кучи в байтах
    current_size: usize,
}

impl HeapAllocator {
    pub fn new(
        frame_allocator: &'static dyn FrameAllocator,
        higher_half_base: PageAlignedVirtualAddress,
    ) -> Self {
        HeapAllocator {
            frame_allocator,
            higher_half_base: higher_half_base.as_usize(),
            free_list_head: None,
            current_size: 0,
        }
    }

    /// Увеличивает емкость кучи, выделяя физические страницы.
    fn expand(&mut self, min_size: usize) -> Result<(), AllocationError> {
        let pages_needed = align_up(min_size, PAGE_SIZE) / PAGE_SIZE;
        let mut remaining = pages_needed;
        let mut total_allocated = 0usize;

        while remaining > 0 {
            let (frame, count) = self
                .frame_allocator
                .allocate_frames(remaining)
                .ok_or(AllocationError::OutOfMemory)?;

            let block_bytes = count * PAGE_SIZE;

            // Вычисляем VA из PA: память уже замаплена линейно
            let va = self.higher_half_base + frame.page_address().as_usize();

            // Создание FreeBlock для этого блока
            let block_size = block_bytes - size_of::<FreeBlock>();
            // SAFETY: `frame` свежевыделен `frame_allocator`-ом, физическая память замаплена линейно
            // в higher half (инвариант аллокатора), поэтому `va` указывает на эксклюзивный валидный
            // регион размера `block_bytes >= size_of::<FreeBlock>()` для записи заголовка.
            unsafe {
                let block_ptr = va as *mut FreeBlock;
                *block_ptr = FreeBlock::from_size(block_size);
                self.add_to_free_list(NonNull::new_unchecked(block_ptr));
            }

            total_allocated += block_bytes;
            remaining -= count;
        }

        self.current_size += total_allocated;
        Ok(())
    }

    /// Находит и удаляет подходящий блок из списка свободных
    fn find_free_block(&mut self, size: usize) -> Option<NonNull<FreeBlock>> {
        let mut current = self.free_list_head;
        let mut prev: Option<NonNull<FreeBlock>> = None;

        while let Some(block_ptr) = current {
            // SAFETY: `block_ptr` - элемент односвязного free-list-а, инвариант аллокатора
            // гарантирует, что все его узлы - валидные `FreeBlock`-и, лежащие в замапленной
            // памяти кучи и эксклюзивно принадлежащие аллокатору, пока находятся в списке.
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

    /// Добавляет блок в список свободных
    fn add_to_free_list(&mut self, block_ptr: NonNull<FreeBlock>) {
        // SAFETY: caller передаёт указатель на полностью владеемый аллокатором `FreeBlock`
        // (либо только что выделенный `expand`-ом, либо только что вытащенный из free-list-а),
        // память замаплена и эксклюзивно доступна - read/write по нему корректны.
        unsafe {
            let mut block = block_ptr.read();
            block.next = self.free_list_head;
            block_ptr.write(block);
            self.free_list_head = Some(block_ptr);
        }
    }

    /// Выделяет память
    ///
    /// Схема выделенного блока:
    /// ```text
    /// [FreeBlock header][padding][ptr to header][user data]
    ///                            ^              ^
    ///                   HEADER_PTR_SIZE         возвращаемый адрес
    /// ```
    pub fn allocate(&mut self, layout: Layout) -> Result<NonNull<u8>, AllocationError> {
        if layout.size() == 0 {
            return Err(AllocationError::InvalidLayout);
        }

        // Учёт запрошенного выравнивания (минимум ALLOC_ALIGN)
        let align = layout.align().max(ALLOC_ALIGN);
        let size = layout.size().max(MIN_ALLOC_SIZE);

        // Необходимое место: данные + указатель на заголовок + padding для выравнивания
        // Максимальный padding = align - 1
        let alloc_size = size + HEADER_PTR_SIZE + align - 1;

        // Поиск в списке свободных блоков
        if let Some(block_ptr) = self.find_free_block(alloc_size) {
            return Ok(Self::setup_allocated_block(block_ptr, align));
        }

        // Не нашли - расширяем
        let needed_size = alloc_size + size_of::<FreeBlock>();
        let expand_size = needed_size.max(PAGE_SIZE);

        self.expand(expand_size)?;

        // Теперь точно найдём
        if let Some(block_ptr) = self.find_free_block(alloc_size) {
            Ok(Self::setup_allocated_block(block_ptr, align))
        } else {
            Err(AllocationError::OutOfMemory)
        }
    }

    /// Подготавливает выделенный блок: записать указатель на заголовок и вернуть выровненный указатель
    fn setup_allocated_block(block_ptr: NonNull<FreeBlock>, align: usize) -> NonNull<u8> {
        // SAFETY: `block_ptr` только что вытащен из free-list-а - это валидный заголовок,
        // за которым следует `block.size` байт принадлежащей аллокатору памяти. Алгоритм
        // `allocate` запросил блок размера `alloc_size = size + HEADER_PTR_SIZE + align - 1`,
        // поэтому смещения `data_start + HEADER_PTR_SIZE`, `aligned_user_addr` и
        // `user_ptr - HEADER_PTR_SIZE` гарантированно лежат внутри этого блока.
        unsafe {
            // Начало области данных (после заголовка)
            let data_start = block_ptr.as_ptr().cast::<u8>().add(size_of::<FreeBlock>());

            // Вычисляем адрес для пользовательских данных с учётом выравнивания
            // Резервирование HEADER_PTR_SIZE байт перед данными для указателя на заголовок
            let min_user_addr = data_start as usize + HEADER_PTR_SIZE;
            let aligned_user_addr = align_up(min_user_addr, align);
            let user_ptr = aligned_user_addr as *mut u8;

            // данными. Используем побайтовое копирование, чтобы не делать cast `*mut u8`
            // в более строго выровненный указатель.
            let header_ptr_value: *mut FreeBlock = block_ptr.as_ptr();
            core::ptr::copy_nonoverlapping(
                core::ptr::from_ref(&header_ptr_value).cast::<u8>(),
                user_ptr.sub(HEADER_PTR_SIZE),
                HEADER_PTR_SIZE,
            );

            NonNull::new_unchecked(user_ptr)
        }
    }

    /// Освобождает память
    ///
    /// Указатель на заголовок хранится перед пользовательскими данными.
    /// При освобождении указатель обнуляется для защиты от double-free.
    pub fn deallocate(&mut self, ptr: NonNull<u8>) {
        let addr = ptr.as_ptr() as usize;
        if addr < self.higher_half_base {
            return;
        }

        // SAFETY: `ptr` лежит в higher-half-области кучи (проверено выше через
        // `addr >= self.higher_half_base`); в `setup_allocated_block` непосредственно перед
        // `ptr` записаны байты `*mut FreeBlock`, поэтому чтение/запись `HEADER_PTR_SIZE` байт
        // по `ptr - HEADER_PTR_SIZE` корректно. Используем побайтовое копирование, чтобы
        // не делать cast `*mut u8` в более строго выровненный указатель.
        unsafe {
            let header_addr = ptr.as_ptr().sub(HEADER_PTR_SIZE);

            let mut block_ptr: *mut FreeBlock = core::ptr::null_mut();
            core::ptr::copy_nonoverlapping(
                header_addr,
                core::ptr::from_mut(&mut block_ptr).cast::<u8>(),
                HEADER_PTR_SIZE,
            );

            if let Some(block) = NonNull::new(block_ptr) {
                // Обнуление указателя на заголовок для защиты от double-free
                let zero: *mut FreeBlock = core::ptr::null_mut();
                core::ptr::copy_nonoverlapping(
                    core::ptr::from_ref(&zero).cast::<u8>(),
                    header_addr,
                    HEADER_PTR_SIZE,
                );

                self.add_to_free_list(block);
            }
            // Если block_ptr уже null (повторный deallocate), ничего не делаем
        }

        // TODO: Реализовать слияние смежных свободных блоков
    }
}
