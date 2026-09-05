//! Kernel-heap: first-fit free-list поверх собственной VA-арены.
//!
//! Heap владеет выделенным kernel-VA-окном и при expand-е делегирует
//! аллокацию + маппинг фреймов `MemoryMapper::map`. Физическая
//! фрагментация фреймов прозрачна: VA-непрерывность арены гарантирует
//! aligned-аллокации.

#![allow(unsafe_code)]

use core::{alloc::Layout, mem::size_of, ptr::NonNull};

use crate::{
    MemFlags, PAGE_SIZE, align::align_up, memory_mapper::MemoryMapper,
    virtual_address::PageAlignedVirtualAddress,
};

const MIN_ALLOC_SIZE: usize = 16;
const ALLOC_ALIGN: usize = 8;
const HEADER_PTR_SIZE: usize = size_of::<*mut FreeBlock>();

#[derive(Debug, Clone)]
pub enum AllocationError {
    OutOfMemory,
    InvalidLayout,
}

#[derive(Clone, Copy, Debug)]
#[repr(C)]
struct FreeBlock {
    size: usize,
    next: Option<NonNull<FreeBlock>>,
}

impl FreeBlock {
    const fn from_size(size: usize) -> Self {
        FreeBlock { size, next: None }
    }

    fn split(&mut self, requested_size: usize) -> Option<NonNull<FreeBlock>> {
        // Заголовок `FreeBlock` лежит в начале свободного блока, а `size`
        // считает только полезную область за ним.
        let aligned_size = align_up(requested_size, ALLOC_ALIGN);
        let block_header_size = size_of::<FreeBlock>();

        let min_remaining = block_header_size + MIN_ALLOC_SIZE;

        if self.size < aligned_size + min_remaining {
            return None;
        }

        let new_block_offset = block_header_size + aligned_size;
        let new_block_size = self.size - aligned_size - block_header_size;

        // Арена не является Rust-аллокацией, и провенанс `&mut self` покрывает только
        // заголовок, поэтому адрес соседнего блока считается через `usize`.
        let new_ptr =
            (core::ptr::from_mut::<FreeBlock>(self) as usize + new_block_offset) as *mut FreeBlock;
        // SAFETY: проверка `self.size >= aligned_size + min_remaining` выше
        // гарантирует, что `new_block_offset` лежит внутри выделенной
        // области, подходящей для записи `FreeBlock`; область принадлежит
        // аллокатору эксклюзивно.
        unsafe { new_ptr.write(FreeBlock::from_size(new_block_size)) };
        // SAFETY: арена лежит в kernel-VA верхней половины, поэтому смещение от
        // адреса `self` не заворачивается через ноль.
        let new_block_ptr = unsafe { NonNull::new_unchecked(new_ptr) };

        self.size = aligned_size;

        Some(new_block_ptr)
    }
}

/// VA-окно `[base, base + max_size)` под heap. Должно лежать целиком
/// в kernel-VA и не пересекаться с другими kernel-маппингами.
#[derive(Copy, Clone, Debug)]
pub struct HeapArena {
    pub base: PageAlignedVirtualAddress,
    pub max_size: usize,
}

impl HeapArena {
    pub const fn new(base: PageAlignedVirtualAddress, max_size: usize) -> Self {
        Self { base, max_size }
    }
}

pub struct HeapAllocator {
    kernel_mapper: &'static (dyn MemoryMapper + Send + Sync),
    arena: HeapArena,
    arena_top: PageAlignedVirtualAddress,
    free_list_head: Option<NonNull<FreeBlock>>,
    current_size: usize,
}

impl HeapAllocator {
    pub fn new(kernel_mapper: &'static (dyn MemoryMapper + Send + Sync), arena: HeapArena) -> Self {
        let arena_top = arena.base;
        HeapAllocator {
            kernel_mapper,
            arena,
            arena_top,
            free_list_head: None,
            current_size: 0,
        }
    }

    fn expand(&mut self, min_size: usize) -> Result<(), AllocationError> {
        let pages_needed = align_up(min_size, PAGE_SIZE.get()) / PAGE_SIZE;
        let bytes = pages_needed * PAGE_SIZE.get();

        let arena_end = self
            .arena
            .base
            .as_usize()
            .checked_add(self.arena.max_size)
            .ok_or(AllocationError::OutOfMemory)?;
        let new_top = self
            .arena_top
            .as_usize()
            .checked_add(bytes)
            .ok_or(AllocationError::OutOfMemory)?;
        if new_top > arena_end {
            return Err(AllocationError::OutOfMemory);
        }

        let block_va = self.arena_top;
        let block_ptr = block_va.as_usize() as *mut FreeBlock;
        // Нулевой адрес в kernel-VA не замаплен, и блок на нём не адресуем.
        let block = NonNull::new(block_ptr).ok_or(AllocationError::OutOfMemory)?;

        self.kernel_mapper
            .map(block_va, pages_needed, &[], MemFlags::kernel_rw())
            .map_err(|_| AllocationError::OutOfMemory)?;

        self.arena_top = block_va.add_blocks(pages_needed);
        debug_assert_eq!(self.arena_top.as_usize(), new_top);
        self.current_size += bytes;

        let block_size = bytes - size_of::<FreeBlock>();
        // SAFETY: `[block_va, block_va + bytes)` только что замаплен подряд
        // как kernel-RW и обнулён mapper-ом; страница выровнена сильнее
        // `FreeBlock`.
        unsafe { block_ptr.write(FreeBlock::from_size(block_size)) };
        self.add_to_free_list(block);

        Ok(())
    }

    fn find_free_block(&mut self, size: usize) -> Option<NonNull<FreeBlock>> {
        let mut current = self.free_list_head;
        let mut prev: Option<NonNull<FreeBlock>> = None;

        while let Some(block_ptr) = current {
            // SAFETY: узлы free-list-а - валидные `FreeBlock` в замапленной
            // арене, эксклюзивно принадлежат аллокатору пока в списке; на время
            // итерации на узел нет других ссылок.
            let block = unsafe { &mut *block_ptr.as_ptr() };

            if block.size >= size {
                let next = block.next;
                if let Some(prev_ptr) = prev {
                    // SAFETY: `prev_ptr` - узел free-list-а, отличный от
                    // `block_ptr`; запись поля `next` не пересекается с `block`.
                    unsafe { (*prev_ptr.as_ptr()).next = next };
                } else {
                    self.free_list_head = next;
                }

                if let Some(new_block_ptr) = block.split(size) {
                    self.add_to_free_list(new_block_ptr);
                }

                return Some(block_ptr);
            }

            prev = current;
            current = block.next;
        }

        None
    }

    fn add_to_free_list(&mut self, block_ptr: NonNull<FreeBlock>) {
        let block_addr = block_ptr.as_ptr() as usize;
        let mut prev: Option<NonNull<FreeBlock>> = None;
        let mut current = self.free_list_head;

        while let Some(current_ptr) = current {
            if current_ptr.as_ptr() as usize >= block_addr {
                break;
            }
            prev = current;
            // SAFETY: `current_ptr` - узел free-list-а: валидный `FreeBlock`
            // в замапленной арене, принадлежащий аллокатору.
            current = unsafe { (*current_ptr.as_ptr()).next };
        }

        // SAFETY: `block_ptr` - валидный `FreeBlock` в замапленной арене,
        // эксклюзивно принадлежит аллокатору.
        unsafe { (*block_ptr.as_ptr()).next = current };
        if let Some(prev_ptr) = prev {
            // SAFETY: `prev_ptr` - узел free-list-а перед точкой вставки,
            // отличный от `block_ptr`.
            unsafe { (*prev_ptr.as_ptr()).next = Some(block_ptr) };
        } else {
            self.free_list_head = Some(block_ptr);
        }

        let coalesce_from = match prev {
            Some(prev_ptr) if Self::blocks_are_adjacent(prev_ptr, block_ptr) => {
                Self::merge_adjacent_blocks(prev_ptr, block_ptr);
                prev_ptr
            }
            _ => block_ptr,
        };

        Self::coalesce_next_blocks(coalesce_from);
    }

    fn blocks_are_adjacent(left_ptr: NonNull<FreeBlock>, right_ptr: NonNull<FreeBlock>) -> bool {
        // SAFETY: `left_ptr` - узел free-list-а, чтение `size` корректно;
        // `right_ptr` не разыменовывается.
        let left_size = unsafe { (*left_ptr.as_ptr()).size };
        let left_end = left_ptr.as_ptr() as usize + size_of::<FreeBlock>() + left_size;
        left_end == right_ptr.as_ptr() as usize
    }

    fn merge_adjacent_blocks(left_ptr: NonNull<FreeBlock>, right_ptr: NonNull<FreeBlock>) {
        // SAFETY: `right_ptr` - узел free-list-а; заголовок копируется, и
        // после поглощения к нему никто не обращается.
        let right = unsafe { *right_ptr.as_ptr() };
        // SAFETY: caller проверил, что `right_ptr` идёт сразу за `left_ptr`;
        // `left_ptr` - узел free-list-а, на него нет других ссылок.
        let left = unsafe { &mut *left_ptr.as_ptr() };
        left.size += size_of::<FreeBlock>() + right.size;
        left.next = right.next;
    }

    fn coalesce_next_blocks(block_ptr: NonNull<FreeBlock>) {
        loop {
            // SAFETY: `block_ptr` - узел free-list-а; читается только `.next`.
            let next = unsafe { (*block_ptr.as_ptr()).next };
            let Some(next_ptr) = next else {
                break;
            };
            if !Self::blocks_are_adjacent(block_ptr, next_ptr) {
                break;
            }
            Self::merge_adjacent_blocks(block_ptr, next_ptr);
        }
    }

    /// Layout выделенного блока:
    /// ```text
    /// [FreeBlock header][padding][ptr to header][user data]
    ///                            ^              ^
    ///                   HEADER_PTR_SIZE         возвращаемый адрес
    /// ```
    pub fn allocate(&mut self, layout: Layout) -> Result<NonNull<u8>, AllocationError> {
        if layout.size() == 0 {
            return Err(AllocationError::InvalidLayout);
        }

        let align = layout.align().max(ALLOC_ALIGN);
        let size = layout.size().max(MIN_ALLOC_SIZE);

        // FreeBlock и data_start всегда 8-выровнены (alignment FreeBlock=8,
        // ALLOC_ALIGN=8), поэтому `min_user_addr mod align` - множитель 8,
        // и худший reach до aligned_user от data_start - ровно `align`
        // байт. Это покрывает HEADER_PTR_SIZE + worst-case padding.
        let alloc_size = size + align;

        if let Some(block_ptr) = self.find_free_block(alloc_size) {
            return Ok(Self::setup_allocated_block(block_ptr, align));
        }

        let needed_size = alloc_size + size_of::<FreeBlock>();
        let expand_size = needed_size.max(PAGE_SIZE.get());

        self.expand(expand_size)?;

        if let Some(block_ptr) = self.find_free_block(alloc_size) {
            Ok(Self::setup_allocated_block(block_ptr, align))
        } else {
            Err(AllocationError::OutOfMemory)
        }
    }

    fn setup_allocated_block(block_ptr: NonNull<FreeBlock>, align: usize) -> NonNull<u8> {
        let data_start = block_ptr.as_ptr() as usize + size_of::<FreeBlock>();
        let min_user_addr = data_start + HEADER_PTR_SIZE;
        let aligned_user_addr = align_up(min_user_addr, align);
        let user_ptr = aligned_user_addr as *mut u8;
        let header_slot = (aligned_user_addr - HEADER_PTR_SIZE) as *mut u8;

        let header_ptr_value: *mut FreeBlock = block_ptr.as_ptr();
        // SAFETY: блок только что вытащен из free-list-а; запрошенный
        // `alloc_size = size + align` гарантирует, что aligned-user-ptr
        // и предшествующий ему `HEADER_PTR_SIZE`-слот лежат внутри блока.
        // Источник - локальная переменная, области не пересекаются.
        unsafe {
            core::ptr::copy_nonoverlapping(
                core::ptr::from_ref(&header_ptr_value).cast::<u8>(),
                header_slot,
                HEADER_PTR_SIZE,
            );
        }
        // SAFETY: `user_ptr` лежит выше ненулевого `block_ptr` внутри того же
        // блока.
        unsafe { NonNull::new_unchecked(user_ptr) }
    }

    /// Освобождает блок и обнуляет header-указатель для защиты от
    /// double-free. Указатели вне арены игнорируются.
    pub fn deallocate(&mut self, ptr: NonNull<u8>) {
        let addr = ptr.as_ptr() as usize;
        let arena_start = self.arena.base.as_usize();
        if addr < arena_start || addr >= self.arena_top.as_usize() {
            return;
        }

        let header_slot = ptr.as_ptr().wrapping_sub(HEADER_PTR_SIZE);
        let mut block_ptr: *mut FreeBlock = core::ptr::null_mut();
        // SAFETY: `ptr` в замапленной арене; перед ним `setup_allocated_block`
        // записал `*mut FreeBlock`. Побайтовое копирование в локальную
        // переменную обходится без cast в более строго выровненный указатель.
        unsafe {
            core::ptr::copy_nonoverlapping(
                header_slot,
                core::ptr::from_mut(&mut block_ptr).cast::<u8>(),
                HEADER_PTR_SIZE,
            );
        }

        let Some(block) = NonNull::new(block_ptr) else {
            return;
        };

        let zero: *mut FreeBlock = core::ptr::null_mut();
        // SAFETY: `header_slot` - тот же слот в замапленной арене, что
        // прочитан выше; источник - локальная переменная.
        unsafe {
            core::ptr::copy_nonoverlapping(
                core::ptr::from_ref(&zero).cast::<u8>(),
                header_slot,
                HEADER_PTR_SIZE,
            );
        }

        self.add_to_free_list(block);
    }

    pub const fn current_size(&self) -> usize {
        self.current_size
    }
}
