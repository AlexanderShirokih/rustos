//! Аллокатор памяти ядра
//!
//! Этот модуль предоставляет аллокатор кучи для ядра, использующий алгоритм
//! first-fit со свободным списком и автоматическим расширением кучи.

use crate::memory::memory_mapper::MemoryMappingError;
use core::alloc::{GlobalAlloc, Layout};
use core::mem::size_of;
use core::ptr::NonNull;
use core::sync::atomic::{AtomicUsize, Ordering};
use memory::memory_backend::{MemoryBackend, MemoryBackendExt};
use memory::physical::PhysicalAddress;
use spin::Mutex;

/// Начальный виртуальный адрес кучи
pub const HEAP_START: usize = 0xFFFF_0000_0000_0000;

/// Начальный размер кучи (1MB)
pub const HEAP_INITIAL_SIZE: usize = 1024 * 1024;

/// Максимальный размер кучи (64MB)
pub const HEAP_MAX_SIZE: usize = 64 * 1024 * 1024;

/// Минимальный размер выделения (16 байт)
const MIN_ALLOC_SIZE: usize = 16;

/// Выравнивание для всех выделений
const ALLOC_ALIGN: usize = 8;

/// Типы ошибок при сбое выделения памяти
#[derive(Debug, Clone)]
pub enum AllocationError {
    OutOfMemory,
    MappingFailed(MemoryMappingError),
    InvalidLayout,
}

/// Трейт маппера памяти для операций виртуальной памяти
pub trait MemoryMapper: Send + Sync {
    /// Отобразить физические фреймы в виртуальную память для кучи
    fn map_heap_frames(&self, start_addr: usize, size: usize) -> Result<(), MemoryMappingError>;
    fn unmap_heap_frames(&self, start_addr: usize, size: usize) -> Result<(), MemoryMappingError>;
}

/// Свободный блок в куче
#[derive(Clone, Copy, Debug)]
struct FreeBlock {
    size: usize,
    next: Option<NonNull<FreeBlock>>,
}

// Safety: FreeBlock используется только внутри нашего контролируемого аллокатора кучи
// с правильной синхронизацией через обертку Mutex
unsafe impl Send for FreeBlock {}
unsafe impl Sync for FreeBlock {}

impl FreeBlock {
    /// Создать новый свободный блок
    const fn new(size: usize) -> Self {
        FreeBlock { size, next: None }
    }

    /// Разделить этот блок, если он достаточно большой для запрошенного размера
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
pub struct HeapAllocator<B: MemoryBackend, M: MemoryMapper> {
    /// Голова списка свободных блоков
    free_list: Option<NonNull<FreeBlock>>,

    /// Текущий размер кучи в байтах
    current_size: usize,

    /// Следующая позиция выделения для расширения кучи
    next_alloc_addr: AtomicUsize,

    /// Маппер памяти для операций виртуальной памяти (указатель вместо владения)
    memory_mapper: *const M,

    /// Бэкенд для чтения/записи памяти
    memory_backend: *const B,

    /// Размер фрейма для маппинга памяти
    frame_size: usize,
}

// Safety: HeapAllocator сырой указатель на backend, но гарантирует его валидность
// через время жизни владельца (MemoryManager)
unsafe impl<B: MemoryBackend, M: MemoryMapper> Send for HeapAllocator<B, M> {}
unsafe impl<B: MemoryBackend, M: MemoryMapper> Sync for HeapAllocator<B, M> {}

impl<B: MemoryBackend, M: MemoryMapper> HeapAllocator<B, M> {
    /// Получить ссылку на memory_backend (безопасно, так как гарантируется владельцем)
    fn memory_backend(&self) -> &B {
        unsafe { &*self.memory_backend }
    }

    /// Получить ссылку на memory_mapper (безопасно, так как гарантируется владельцем)
    fn memory_mapper(&self) -> &M {
        unsafe { &*self.memory_mapper }
    }

    /// Создать новый аллокатор кучи
    pub fn new(memory_mapper: &M, memory_backend: &B, frame_size: usize) -> Self {
        HeapAllocator {
            free_list: None,
            current_size: 0,
            next_alloc_addr: AtomicUsize::new(HEAP_START),
            memory_mapper: memory_mapper as *const M,
            memory_backend: memory_backend as *const B,
            frame_size,
        }
    }

    /// Инициализировать аллокатор с начальным пространством кучи
    pub fn init(&mut self) -> Result<(), AllocationError> {
        self.expand_heap(HEAP_INITIAL_SIZE)
    }

    /// Расширить кучу путем выделения дополнительной памяти
    fn expand_heap(&mut self, additional_size: usize) -> Result<(), AllocationError> {
        let aligned_size = align_up(additional_size, self.frame_size);

        // Проверяем, не превысит ли расширение максимальный размер кучи
        if self.current_size + aligned_size > HEAP_MAX_SIZE {
            return Err(AllocationError::OutOfMemory);
        }

        let mut start_addr = self.next_alloc_addr.load(Ordering::Relaxed);

        loop {
            // Отображаем физические фреймы в виртуальную область кучи
            let error = self
                .memory_mapper()
                .map_heap_frames(start_addr, aligned_size)
                .err();

            match error {
                Some(e) => match e {
                    MemoryMappingError::AlreadyMapped => {
                        start_addr = start_addr + self.frame_size;
                    }
                    _ => return Err(AllocationError::MappingFailed(e)),
                },

                None => break,
            }
        }

        // Создаем новый свободный блок для расширенной области
        let block_addr = PhysicalAddress(start_addr);
        let block_size = aligned_size - size_of::<FreeBlock>();
        let new_block = FreeBlock::new(block_size);

        // Записываем новый блок в память
        self.memory_backend().write(block_addr, new_block);

        // Добавляем в список свободных блоков
        self.add_to_free_list(start_addr);

        // Обновляем состояние аллокатора
        self.current_size += aligned_size;
        self.next_alloc_addr
            .store(start_addr + aligned_size, Ordering::Relaxed);

        Ok(())
    }

    /// Добавить блок в список свободных
    fn add_to_free_list(&mut self, addr: usize) {
        let block_ptr = NonNull::new(addr as *mut FreeBlock).expect("Invalid address");

        // Читаем блок, обновляем его указатель next и записываем обратно
        let mut block: FreeBlock = self.memory_backend().read(PhysicalAddress(addr));
        block.next = self.free_list;
        self.memory_backend().write(PhysicalAddress(addr), block);

        // Обновляем голову списка свободных блоков
        self.free_list = Some(block_ptr);
    }

    /// Найти и удалить подходящий блок из списка свободных
    fn find_free_block(&mut self, size: usize) -> Option<NonNull<FreeBlock>> {
        let mut current = self.free_list;
        let mut prev: Option<NonNull<FreeBlock>> = None;

        while let Some(block_ptr) = current {
            let addr = block_ptr.as_ptr() as usize;
            let mut block: FreeBlock = self.memory_backend().read(PhysicalAddress(addr));

            // Проверяем, достаточно ли велик этот блок
            if block.size >= size {
                // Удаляем из списка свободных
                if let Some(prev_ptr) = prev {
                    let mut prev_block: FreeBlock = self
                        .memory_backend()
                        .read(PhysicalAddress(prev_ptr.as_ptr() as usize));
                    prev_block.next = block.next;
                    self.memory_backend()
                        .write(PhysicalAddress(prev_ptr.as_ptr() as usize), prev_block);
                } else {
                    self.free_list = block.next;
                }

                // Разделяем блок, если он значительно больше, чем нужно
                if let Some(new_block_ptr) = block.split(size) {
                    let new_addr = new_block_ptr.as_ptr() as usize;
                    self.add_to_free_list(new_addr);
                }

                // Записываем обратно выделенный блок
                self.memory_backend().write(PhysicalAddress(addr), block);
                return Some(block_ptr);
            }

            // Переходим к следующему блоку
            prev = current;
            current = block.next;
        }

        None
    }

    /// Выделить память с заданным layout
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
        let expand_size = needed_size.max(self.frame_size);

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

    /// Получить текущую статистику кучи
    pub fn stats(&self) -> HeapStats {
        HeapStats {
            total_size: self.current_size,
            max_size: HEAP_MAX_SIZE,
        }
    }
}

/// Статистика кучи для мониторинга
#[derive(Debug, Clone, Copy)]
pub struct HeapStats {
    pub total_size: usize,
    pub max_size: usize,
}

/// Глобальный аллокатор ядра
pub struct KernelAllocator<B: MemoryBackend, M: MemoryMapper> {
    inner: Mutex<Option<HeapAllocator<B, M>>>,
}

impl<'a, B: MemoryBackend, M: MemoryMapper> KernelAllocator<B, M> {
    /// Создать новый аллокатор ядра
    pub const fn new() -> Self {
        KernelAllocator {
            inner: Mutex::new(None),
        }
    }

    /// Получить статистику кучи
    pub fn stats(&self) -> Option<HeapStats> {
        let inner = self.inner.lock();
        inner.as_ref().map(|allocator| allocator.stats())
    }
}

unsafe impl<B: MemoryBackend, M: MemoryMapper> GlobalAlloc for KernelAllocator<B, M> {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let mut inner = self.inner.lock();

        if let Some(ref mut allocator) = *inner {
            match allocator.allocate(layout) {
                Ok(ptr) => ptr.as_ptr(),
                Err(_) => core::ptr::null_mut(),
            }
        } else {
            // Возвращаем null, если аллокатор не инициализирован
            core::ptr::null_mut()
        }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, _layout: Layout) {
        if ptr.is_null() {
            return;
        }

        let mut inner = self.inner.lock();

        if let Some(ref mut allocator) = *inner {
            if let Some(non_null_ptr) = NonNull::new(ptr) {
                allocator.deallocate(non_null_ptr);
            }
        }
        // Если аллокатор не инициализирован, молча игнорируем освобождение
    }
}

/// Вспомогательная функция для выравнивания вверх до указанного выравнивания
const fn align_up(addr: usize, align: usize) -> usize {
    (addr + align - 1) & !(align - 1)
}
