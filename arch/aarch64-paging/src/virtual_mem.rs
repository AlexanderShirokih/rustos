//! Управление виртуальной памятью
//!
//! Модуль инкапсулирует четырёхуровневую таблицу страниц архитектуры
//! AArch64. Он предоставляет примитивы (`Page`, `PageTableEntry`,
//! `PageTable`) и высокоуровневый `PageTableManager`, который отвечает за
//! построение и обслуживание иерархии, identity-маппинг ключевых регионов и
//! синхронизацию с абстрактным `MemoryBackend`.

use crate::entry_flags::EntryFlags;
use crate::layout::{MemoryLayout, MemoryRegion};
use crate::virtual_address::{VirtualAddress, VirtualAddressExt};
use core::ops::{Index, IndexMut};
use kernel_core::console::console;
use kernel_core::debug;
use memory::memory_backend::{MemoryBackend, MemoryPtr};
use memory::physical::{Frame, PageAlignedAddress, PhysicalAddress};
use memory::physical_manager::{FrameAllocator, ReserveFrameError};

/// Страница виртуальной памяти
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Page {
    number: usize,
}

const PAGE_SIZE: usize = 4096usize;

impl Page {
    /// Создать новую страницу из номера страницы
    pub const fn new(number: usize) -> Self {
        Page { number }
    }

    /// Создать страницу, содержащую заданный виртуальный адрес
    pub fn containing_address(address: VirtualAddress) -> Self {
        Page {
            number: address.as_usize() / PAGE_SIZE,
        }
    }

    /// Получить начальный виртуальный адрес этой страницы
    pub fn start_address(&self) -> VirtualAddress {
        VirtualAddress(self.number * PAGE_SIZE)
    }

    /// Получить номер страницы
    pub const fn number(&self) -> usize {
        self.number
    }

    pub const fn add(self, offset: usize) -> Self {
        Page {
            number: self.number + offset,
        }
    }

    /// Получить следующую страницу
    pub fn next(&self) -> Self {
        Page::new(self.number + 1)
    }

    /// Создать диапазон страниц
    pub fn range_inclusive(start: Page, end: Page) -> impl Iterator<Item = Page> {
        (start.number..=end.number).map(Page::new)
    }

    /// Получить индексы таблиц страниц для этой страницы
    pub fn table_indices(&self) -> [usize; 4] {
        self.start_address().page_table_indices()
    }
}

/// Запись таблицы страниц для aarch64
#[derive(Debug, Clone, Copy)]
#[repr(transparent)]
pub struct PageTableEntry(u64);

impl PageTableEntry {
    const PHYS_ADDR_MASK: u64 = 0x0000_FFFF_FFFF_F000;

    pub const fn new() -> Self {
        PageTableEntry(0)
    }

    const fn with_address_and_flags(address: PageAlignedAddress, flags: EntryFlags) -> Self {
        let masked_addr = (address.as_usize() as u64) & Self::PHYS_ADDR_MASK;
        PageTableEntry(masked_addr | flags.bits())
    }

    pub fn new_frame(frame: Frame, flags: EntryFlags) -> Self {
        let addr = frame.page_address();
        Self::with_address_and_flags(addr, flags)
    }

    fn new_table(table_frame: Frame) -> Self {
        let flags = EntryFlags::combine(&[EntryFlags::VALID, EntryFlags::TABLE]);
        let addr = table_frame.page_address();
        Self::with_address_and_flags(addr, flags)
    }

    pub fn is_valid(&self) -> bool {
        (self.0 & EntryFlags::VALID.bits()) != 0
    }

    pub fn is_table(&self) -> bool {
        self.is_valid() && (self.0 & EntryFlags::TABLE.bits()) != 0
    }

    fn is_block(&self) -> bool {
        self.is_valid() && (self.0 & EntryFlags::TABLE.bits()) == 0
    }

    /// Получить физический адрес, на который указывает эта запись
    pub fn address(&self) -> PhysicalAddress {
        ((self.0 & Self::PHYS_ADDR_MASK) as usize).into()
    }

    /// Получить фрейм, на который указывает эта запись
    pub fn frame(&self) -> Frame {
        Frame::containing_address(self.address())
    }

    /// Установить запись в новое значение
    pub fn set(&mut self, entry: PageTableEntry) {
        self.0 = entry.0;
    }

    /// Очистить запись
    pub fn clear(&mut self) {
        self.0 = 0;
    }
}

/// Таблица страниц
#[repr(align(4096))]
#[derive(Copy, Clone)]
pub struct PageTable {
    entries: [PageTableEntry; 512],
}

impl PageTable {
    pub fn new() -> Self {
        PageTable {
            entries: [PageTableEntry::new(); 512],
        }
    }
}

impl Index<usize> for PageTable {
    type Output = PageTableEntry;

    fn index(&self, index: usize) -> &Self::Output {
        &self.entries[index]
    }
}

impl IndexMut<usize> for PageTable {
    fn index_mut(&mut self, index: usize) -> &mut Self::Output {
        &mut self.entries[index]
    }
}

#[derive(Debug, Clone)]
pub enum VmError {
    /// Страница уже отображена
    AlreadyMapped,
    /// Страница не отображена
    NotMapped,
    /// Невозможно отобразить страницу из-за существующего блокового отображения
    BlockMappingExists,
    /// Не удалось выделить фрейм для корневой таблицы
    RootFrameAllocationFailed,

    /// Не удалось выделить фрейм для 1:1 мапинга
    DmaFrameAllocationFailed {
        reserver_frame_error: ReserveFrameError,
    },

    /// Не удалось выделить фрейм для таблицы
    FrameAllocationFailed { level: usize, index: usize },
    /// Найдена недействительная запись таблицы
    InvalidTableEntry,
    /// Недействительный уровень таблицы страниц
    InvalidLevel,
}

/// Менеджер таблиц страниц для aarch64
pub struct PageTableManager<'a, FA: FrameAllocator, B: MemoryBackend> {
    /// Корневая таблица страниц (уровень 0)
    root_table: Frame,

    /// Бэкенд для операций с памятью
    backend: &'a B,

    frame_allocator: &'a FA,

    /// Регион памяти кучи
    heap: MemoryRegion<PageAlignedAddress>,
}

impl<'a, FA: FrameAllocator, B: MemoryBackend> PageTableManager<'a, FA, B> {
    fn table_address(&self, frame: Frame) -> PageAlignedAddress {
        frame.page_address()
    }

    fn write_table(&self, frame: Frame, table: PageTable) {
        let addr = self.table_address(frame).as_physical_address();

        debug!(
            console(),
            "write table frame: {:#x}. addr: {:#x}",
            frame.number(),
            addr.as_usize()
        );

        self.backend().write(addr, table);
    }

    fn entry_address(&self, frame: Frame, index: usize) -> PhysicalAddress {
        let base = self.table_address(frame).as_usize();
        PhysicalAddress::from(base + index * size_of::<PageTableEntry>())
    }

    fn read_entry(&self, frame: Frame, index: usize) -> PageTableEntry {
        let addr = self.entry_address(frame, index);
        self.backend().read::<PageTableEntry>(addr)
    }

    fn write_entry(&self, frame: Frame, index: usize, entry: PageTableEntry) {
        let addr = self.entry_address(frame, index);
        self.backend().write::<PageTableEntry>(addr, entry);
    }

    fn alloc_table_frame(&self, level: usize, index: usize) -> Result<Frame, VmError> {
        let frame = self
            .frame_allocator
            .allocate_frame()
            .ok_or(VmError::FrameAllocationFailed { level, index })?;

        self.write_table(frame, PageTable::new());

        Ok(frame)
    }

    fn walk_to_leaf(&self, indices: [usize; 4], create_missing: bool) -> Result<Frame, VmError> {
        let mut table_frame = self.root_table;

        for level in 0..3 {
            let index = indices[level];

            debug!(
                console(),
                "reading entry at level {}; frame={:#x}",
                level,
                table_frame.page_address().as_usize()
            );

            let entry = self.read_entry(table_frame, index);

            debug!(console(), "entry: {:?}", entry);

            if entry.is_table() {
                table_frame = entry.frame();
                continue;
            }

            if entry.is_valid() {
                return Err(VmError::BlockMappingExists);
            }

            if !create_missing {
                return Err(VmError::NotMapped);
            }

            debug!(
                console(),
                "prepare to alloc. level={}, index={}", level, index
            );

            let child_frame = self.alloc_table_frame(level, index)?;

            debug!(
                console(),
                "child frame: {:?}; writing at: {:#x}, {}",
                child_frame,
                table_frame.number(),
                index
            );

            self.write_entry(table_frame, index, PageTableEntry::new_table(child_frame));

            debug!(
                console(),
                "allocated child frame at level={}. idx={}. frame={:#x}",
                level,
                index,
                child_frame.number()
            );

            // Настраиваем 1:1 маппинг для доступа к таблице после включения MMU
            self.identity_map_region(
                self.table_address(child_frame),
                self.table_address(child_frame).next_aligned(),
                self.heap_flags(),
            )?;

            table_frame = child_frame;
        }

        Ok(table_frame)
    }

    fn with_leaf_entry<F, R>(
        &self,
        page: Page,
        create_missing: bool,
        mut action: F,
    ) -> Result<R, VmError>
    where
        F: FnMut(&mut PageTableEntry) -> Result<R, VmError>,
    {
        let indices = page.table_indices();

        debug!(
            console(),
            "indices: {:?} for page {:#x}",
            indices,
            page.number()
        );

        let table_frame = match self.walk_to_leaf(indices, create_missing) {
            Err(VmError::BlockMappingExists) if !create_missing => return Err(VmError::NotMapped),
            other => other?,
        };

        let leaf_idx = indices[3];
        let mut entry = self.read_entry(table_frame, leaf_idx);
        let result = action(&mut entry)?;
        self.write_entry(table_frame, leaf_idx, entry);

        Ok(result)
    }

    pub fn heap_flags(&self) -> EntryFlags {
        self.heap.flags
    }

    fn backend(&self) -> &B {
        self.backend
    }

    pub fn new(
        frame_allocator: &'a FA,
        backend: &'a B,
        heap: MemoryRegion<PageAlignedAddress>,
    ) -> Result<Self, VmError> {
        let root_table = frame_allocator
            .allocate_frame()
            .ok_or(VmError::RootFrameAllocationFailed)?;

        debug!(
            console(),
            "Allocated root page: 0x{:x}",
            root_table.page_address().as_usize()
        );

        let root_table_address = root_table.page_address();
        let root_ptr: MemoryPtr<PageTable> = root_table_address.as_physical_address().into();

        root_ptr.write(backend, PageTable::new());

        let page_table_manager = PageTableManager {
            root_table,
            frame_allocator,
            backend,
            heap,
        };

        // Корневая таблица должна быть замаплена, чтобы иметь доступ к ней после включения пэйджинга
        page_table_manager.identity_map_region(
            root_table_address,
            root_table_address.next_aligned(),
            page_table_manager.heap.flags,
        )?;

        Ok(page_table_manager)
    }

    /// Включить или выключить режим виртуальной памяти
    pub fn enable_virtual_mode(&self) {
        self.backend()
            .enable_virtual_mode(self.root_table.page_address().as_physical_address());
    }

    /// Отобразить регион памяти с идентичным маппингом
    fn identity_map_region(
        &self,
        from_inclusive: PageAlignedAddress,
        to_exclusive: PageAlignedAddress,
        flags: EntryFlags,
    ) -> Result<(), VmError> {
        debug_assert!(from_inclusive <= to_exclusive);

        let from_page = Page::containing_address(VirtualAddress::new(from_inclusive.as_usize()));
        let from_frame = Frame::containing_address(from_inclusive);
        let to_frame = Frame::containing_address(to_exclusive);

        let count = to_frame.number() - from_frame.number();
        self.map_range(from_page, from_frame, count, flags)?;

        Ok(())
    }

    /// Отобразить виртуальную страницу на физический фрейм
    pub fn map(&self, page: Page, frame: Frame, flags: EntryFlags) -> Result<(), VmError> {
        self.map_creating(page, frame, flags)?;

        let phys_addr = frame.page_address();
        self.backend()
            .clean_page_cache(phys_addr.as_physical_address());

        Ok(())
    }

    fn map_creating(&self, page: Page, frame: Frame, flags: EntryFlags) -> Result<(), VmError> {
        self.with_leaf_entry(page, true, |entry| {
            if entry.is_valid() {
                return Err(VmError::AlreadyMapped);
            }

            // На уровне L3 автоматически устанавливаем VALID, ACCESS и PAGE
            let l3_flags = flags
                .set(EntryFlags::VALID)
                .set(EntryFlags::ACCESS)
                .set(EntryFlags::PAGE);

            entry.set(PageTableEntry::new_frame(frame, l3_flags));

            Ok(())
        })
    }

    /// Размапить виртуальную страницу
    pub fn unmap(&self, page: Page) -> Result<Frame, VmError> {
        self.with_leaf_entry(page, false, |entry| {
            if !entry.is_valid() {
                return Err(VmError::NotMapped);
            }

            let frame = entry.frame();
            entry.clear();
            Ok(frame)
        })
    }

    /// Отобразить диапазон страниц на диапазон фреймов
    pub fn map_range(
        &self,
        from_page: Page,
        from_frame: Frame,
        size: usize,
        flags: EntryFlags,
    ) -> Result<(), VmError> {
        for i in 0..size {
            let page = from_page.add(i);
            let frame = from_frame.add(i);
            self.map_creating(page, frame, flags)?;
        }

        self.backend().invalidate_cache();

        Ok(())
    }

    /// Транслировать виртуальный адрес в физический адрес
    pub fn translate(&self, addr: VirtualAddress) -> Option<PhysicalAddress> {
        let indices = addr.page_table_indices();
        let mut table_frame = self.root_table;

        for level in 0..3 {
            let idx = indices[level];
            let entry = self.read_entry(table_frame, idx);
            if !entry.is_valid() {
                return None;
            }

            if level > 0 && entry.is_block() {
                let block_size = match level {
                    1 => 1024 * 1024 * 1024, // 1GB
                    2 => 2 * 1024 * 1024,    // 2MB
                    _ => return None,
                };
                let block_mask = block_size - 1;
                let offset = addr.as_usize() & block_mask;
                return Some(entry.address().add(offset));
            }

            if !entry.is_table() {
                return None;
            }

            table_frame = entry.frame();
        }

        let idx = indices[3];
        let entry = self.read_entry(table_frame, idx);

        if !entry.is_valid() {
            return None;
        }

        Some(entry.address().add(addr.page_offset()))
    }

    /// Проверить, отображен ли виртуальный адрес
    pub fn is_mapped(&self, addr: VirtualAddress) -> bool {
        self.translate(addr).is_some()
    }

    /// Получить фрейм корневой таблицы страниц
    pub fn root_frame(&self) -> Frame {
        self.root_table
    }
}

/// Инициализировать систему виртуальной памяти без статических времен жизни
pub fn create_page_table_manager<'a, FA: FrameAllocator, B: MemoryBackend>(
    frame_allocator: &'a FA,
    memory_backend: &'a B,
    memory_layout: MemoryLayout,
    identity_map_regions: &[MemoryRegion<PageAlignedAddress>],
) -> Result<PageTableManager<'a, FA, B>, VmError> {
    // Создаем менеджер таблиц страниц
    let page_table_manager =
        PageTableManager::new(frame_allocator, memory_backend, memory_layout.heap)?;

    // Отображаем все прямые регионы с идентичным маппингом
    for region in identity_map_regions {
        debug!(
            console(),
            "Identity-mapping region: {} ({:#x} - {:#x})",
            region.label,
            region.start.as_usize(),
            region.end.as_usize()
        );

        page_table_manager.identity_map_region(region.start, region.end, region.flags)?;
    }

    // Маппим пямять под нужны собственного аллокатора
    let self_area = frame_allocator.get_self_area();
    debug!(
        console(),
        "Identity-mapping region: {} ({:#x} - {:#x})",
        "Self area",
        self_area.start().as_usize(),
        self_area.end().as_usize()
    );

    page_table_manager.identity_map_region(
        self_area.start(),
        self_area.end(),
        page_table_manager.heap_flags(),
    )?;

    Ok(page_table_manager)
}
