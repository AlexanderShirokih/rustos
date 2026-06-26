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

/// Free-list-узел: лежит в начале каждого свободного блока кучи.
#[derive(Clone, Copy, Debug)]
#[repr(C)]
struct FreeBlock {
    /// Размер полезной области блока в байтах, без заголовка.
    size: usize,
    next: Option<NonNull<FreeBlock>>,
}

impl FreeBlock {
    const fn from_size(size: usize) -> Self {
        FreeBlock { size, next: None }
    }

    /// Откалывает хвост блока в новый `FreeBlock`, оставляя в `self`
    /// ровно `align_up(requested_size, ALLOC_ALIGN)` байт. `None`, если
    /// остатка не хватает на новый header + `MIN_ALLOC_SIZE`.
    fn split(&mut self, requested_size: usize) -> Option<NonNull<FreeBlock>> {
        let aligned_size = align_up(requested_size, ALLOC_ALIGN);
        let block_header_size = size_of::<FreeBlock>();

        let min_remaining = block_header_size + MIN_ALLOC_SIZE;

        if self.size < aligned_size + min_remaining {
            return None;
        }

        let new_block_offset = block_header_size + aligned_size;
        let new_block_size = self.size - aligned_size - block_header_size;

        // SAFETY: проверка `self.size >= aligned_size + min_remaining` выше
        // гарантирует, что `new_block_offset` лежит внутри выделенной
        // области, подходящей для записи `FreeBlock`.
        let new_block_ptr = unsafe {
            let base_ptr = core::ptr::from_mut::<FreeBlock>(self) as usize;
            let new_ptr = (base_ptr + new_block_offset) as *mut FreeBlock;

            *new_ptr = FreeBlock::from_size(new_block_size);

            NonNull::new_unchecked(new_ptr)
        };

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
    /// VA первой ещё не замапленной страницы арены; растёт только вверх.
    arena_top: usize,
    free_list_head: Option<NonNull<FreeBlock>>,
    current_size: usize,
}

impl HeapAllocator {
    pub fn new(kernel_mapper: &'static (dyn MemoryMapper + Send + Sync), arena: HeapArena) -> Self {
        let arena_top = arena.base.as_usize();
        HeapAllocator {
            kernel_mapper,
            arena,
            arena_top,
            free_list_head: None,
            current_size: 0,
        }
    }

    /// Маппит подряд `align_up(min_size, PAGE_SIZE)` байт в следующий
    /// VA-слот арены. Атомарность leaf-страниц при partial-OOM
    /// обеспечивает `MemoryMapper::map`.
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
            .checked_add(bytes)
            .ok_or(AllocationError::OutOfMemory)?;
        if new_top > arena_end {
            return Err(AllocationError::OutOfMemory);
        }

        let block_va_start = self.arena_top;
        let block_va = PageAlignedVirtualAddress::from_usize(block_va_start)
            .expect("arena top remains 4K-aligned");

        self.kernel_mapper
            .map(block_va, pages_needed, &[], MemFlags::kernel_rw())
            .map_err(|_| AllocationError::OutOfMemory)?;

        self.arena_top = new_top;
        self.current_size += bytes;

        let block_size = bytes - size_of::<FreeBlock>();
        // SAFETY: `[block_va_start, block_va_start + bytes)` только что
        // замаплен подряд как kernel-RW и обнулён mapper-ом.
        unsafe {
            let block_ptr = block_va_start as *mut FreeBlock;
            *block_ptr = FreeBlock::from_size(block_size);
            self.add_to_free_list(NonNull::new_unchecked(block_ptr));
        }

        Ok(())
    }

    fn find_free_block(&mut self, size: usize) -> Option<NonNull<FreeBlock>> {
        let mut current = self.free_list_head;
        let mut prev: Option<NonNull<FreeBlock>> = None;

        while let Some(block_ptr) = current {
            // SAFETY: узлы free-list-а - валидные `FreeBlock` в замапленной
            // арене, эксклюзивно принадлежат аллокатору пока в списке.
            unsafe {
                let block = block_ptr.as_ptr();

                if (*block).size >= size {
                    if let Some(prev_ptr) = prev {
                        (*prev_ptr.as_ptr()).next = (*block).next;
                    } else {
                        self.free_list_head = (*block).next;
                    }

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

    /// Вставляет блок в free-list по возрастанию адреса и сливает с
    /// VA-смежными соседями.
    fn add_to_free_list(&mut self, block_ptr: NonNull<FreeBlock>) {
        // SAFETY: `block_ptr` - валидный `FreeBlock` в замапленной арене,
        // эксклюзивно принадлежит аллокатору.
        unsafe {
            (*block_ptr.as_ptr()).next = None;

            let block_addr = block_ptr.as_ptr() as usize;
            let mut prev: Option<NonNull<FreeBlock>> = None;
            let mut current = self.free_list_head;

            while let Some(current_ptr) = current {
                if current_ptr.as_ptr() as usize >= block_addr {
                    break;
                }
                prev = current;
                current = (*current_ptr.as_ptr()).next;
            }

            (*block_ptr.as_ptr()).next = current;
            if let Some(prev_ptr) = prev {
                (*prev_ptr.as_ptr()).next = Some(block_ptr);
            } else {
                self.free_list_head = Some(block_ptr);
            }

            let coalesce_from = if let Some(prev_ptr) = prev {
                if Self::blocks_are_adjacent(prev_ptr, block_ptr) {
                    Self::merge_adjacent_blocks(prev_ptr, block_ptr);
                    prev_ptr
                } else {
                    block_ptr
                }
            } else {
                block_ptr
            };

            Self::coalesce_next_blocks(coalesce_from);
        }
    }

    fn blocks_are_adjacent(left_ptr: NonNull<FreeBlock>, right_ptr: NonNull<FreeBlock>) -> bool {
        // SAFETY: оба указателя - узлы free-list-а; чтение `left.size`
        // корректно, `right_ptr` не разыменовывается.
        unsafe {
            let left_end =
                left_ptr.as_ptr() as usize + size_of::<FreeBlock>() + (*left_ptr.as_ptr()).size;
            left_end == right_ptr.as_ptr() as usize
        }
    }

    fn merge_adjacent_blocks(left_ptr: NonNull<FreeBlock>, right_ptr: NonNull<FreeBlock>) {
        // SAFETY: caller проверил, что `right_ptr` идёт сразу за `left_ptr`;
        // оба - узлы free-list-а аллокатора.
        unsafe {
            (*left_ptr.as_ptr()).size += size_of::<FreeBlock>() + (*right_ptr.as_ptr()).size;
            (*left_ptr.as_ptr()).next = (*right_ptr.as_ptr()).next;
        }
    }

    fn coalesce_next_blocks(block_ptr: NonNull<FreeBlock>) {
        // SAFETY: `block_ptr` - узел free-list-а; читается только `.next`
        // и поглощается, если соседи VA-смежны.
        unsafe {
            while let Some(next_ptr) = (*block_ptr.as_ptr()).next {
                if !Self::blocks_are_adjacent(block_ptr, next_ptr) {
                    break;
                }
                Self::merge_adjacent_blocks(block_ptr, next_ptr);
            }
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
        // SAFETY: блок только что вытащен из free-list-а; запрошенный
        // `alloc_size = size + align` гарантирует, что aligned-user-ptr
        // и предшествующий ему `HEADER_PTR_SIZE`-слот лежат внутри блока.
        unsafe {
            let data_start = block_ptr.as_ptr().cast::<u8>().add(size_of::<FreeBlock>());

            let min_user_addr = data_start as usize + HEADER_PTR_SIZE;
            let aligned_user_addr = align_up(min_user_addr, align);
            let user_ptr = aligned_user_addr as *mut u8;

            let header_ptr_value: *mut FreeBlock = block_ptr.as_ptr();
            core::ptr::copy_nonoverlapping(
                core::ptr::from_ref(&header_ptr_value).cast::<u8>(),
                user_ptr.sub(HEADER_PTR_SIZE),
                HEADER_PTR_SIZE,
            );

            NonNull::new_unchecked(user_ptr)
        }
    }

    /// Освобождает блок и обнуляет header-указатель для защиты от
    /// double-free. Указатели вне арены игнорируются.
    pub fn deallocate(&mut self, ptr: NonNull<u8>) {
        let addr = ptr.as_ptr() as usize;
        let arena_start = self.arena.base.as_usize();
        if addr < arena_start || addr >= self.arena_top {
            return;
        }

        // SAFETY: `ptr` в замапленной арене; перед ним setup_allocated_block
        // записал `*mut FreeBlock`. Побайтовое копирование избегает cast
        // в более строго выровненный указатель.
        unsafe {
            let header_addr = ptr.as_ptr().sub(HEADER_PTR_SIZE);

            let mut block_ptr: *mut FreeBlock = core::ptr::null_mut();
            core::ptr::copy_nonoverlapping(
                header_addr,
                core::ptr::from_mut(&mut block_ptr).cast::<u8>(),
                HEADER_PTR_SIZE,
            );

            if let Some(block) = NonNull::new(block_ptr) {
                let zero: *mut FreeBlock = core::ptr::null_mut();
                core::ptr::copy_nonoverlapping(
                    core::ptr::from_ref(&zero).cast::<u8>(),
                    header_addr,
                    HEADER_PTR_SIZE,
                );

                self.add_to_free_list(block);
            }
        }
    }

    pub const fn current_size(&self) -> usize {
        self.current_size
    }
}
