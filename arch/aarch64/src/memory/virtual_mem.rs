//! Управление виртуальной памятью

use crate::memory::entry_flags::EntryFlags;
use crate::memory::layout::{MemoryLayout, MemoryRegion};
use crate::memory::virtual_address::{VirtualAddress, VirtualAddressExt};
use core::ops::{Index, IndexMut};
use kernel_core::console::console;
use kernel_core::{debug, info};
use memory::memory_backend::{MemoryBackend, MemoryBackendExt, MemoryPtr};
use memory::physical::{Frame, PageAlignedAddress, PhysicalAddress};
use memory::physical_manager::{FrameAllocator, ReserveFrameError};

/// Страница виртуальной памяти
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Page {
    number: usize,
}

impl Page {
    /// Создать новую страницу из номера страницы
    pub const fn new(number: usize) -> Self {
        Page { number }
    }

    /// Создать страницу, содержащую заданный виртуальный адрес
    pub fn containing_address(address: VirtualAddress, page_size: usize) -> Self {
        Page {
            number: address.as_usize() / page_size,
        }
    }

    /// Получить начальный виртуальный адрес этой страницы
    pub fn start_address(&self, page_size: usize) -> VirtualAddress {
        VirtualAddress(self.number * page_size)
    }

    /// Получить номер страницы
    pub const fn number(&self) -> usize {
        self.number
    }

    /// Получить следующую страницу
    pub fn next(&self) -> Self {
        Page::new(self.number + 1)
    }

    /// Создать диапазон страниц
    pub fn range_inclusive(start: Page, end: Page) -> impl Iterator<Item = Page> {
        (start.number..=end.number).map(Page::new)
    }
}

/// Запись таблицы страниц для aarch64
#[derive(Debug, Clone, Copy)]
#[repr(transparent)]
pub struct PageTableEntry(u64);

impl PageTableEntry {
    pub const fn new() -> Self {
        PageTableEntry(0)
    }

    fn new_frame(frame: Frame, flags: EntryFlags, frame_size: usize) -> Self {
        let addr = frame.start_address(frame_size).as_usize() as u64;
        // Убеждаемся, что адрес правильно выровнен и находится в допустимом диапазоне
        let masked_addr = addr & 0x0000_FFFF_FFFF_F000;
        PageTableEntry(masked_addr | flags.bits())
    }

    fn new_table(table_frame: Frame, frame_size: usize) -> Self {
        let addr = table_frame.start_address(frame_size).as_usize() as u64;
        let masked_addr = addr & 0x0000_FFFF_FFFF_F000;
        let flags = EntryFlags::combine(&[EntryFlags::VALID, EntryFlags::TABLE]);
        PageTableEntry(masked_addr | flags.bits())
    }

    fn is_valid(&self) -> bool {
        (self.0 & EntryFlags::VALID.bits()) != 0
    }

    fn is_table(&self) -> bool {
        self.is_valid() && (self.0 & EntryFlags::TABLE.bits()) != 0
    }

    fn is_block(&self) -> bool {
        self.is_valid() && (self.0 & EntryFlags::TABLE.bits()) == 0
    }

    /// Получить физический адрес, на который указывает эта запись
    pub fn address(&self) -> PhysicalAddress {
        ((self.0 & 0x0000_FFFF_FFFF_F000) as usize).into()
    }

    /// Получить фрейм, на который указывает эта запись
    pub fn frame(&self, frame_size: usize) -> Frame {
        Frame::containing_address(self.address(), frame_size)
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

    /// Очистить все записи в таблице страниц
    pub fn clear(&mut self) {
        for entry in &mut self.entries {
            entry.clear();
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
    frame_size: usize,
    /// Корневая таблица страниц (уровень 0)
    root_table: Frame,

    /// Бэкенд для операций с памятью
    backend: &'a B,

    frame_allocator: &'a FA,

    /// Регион памяти кучи
    pub heap: MemoryRegion<PageAlignedAddress>,
}

// Safety: PageTableManager хранит сырые указатели, но гарантирует их валидность
// через время жизни владельца (MemoryManager)
unsafe impl<FA: FrameAllocator, B: MemoryBackend> Send for PageTableManager<'_, FA, B> {}
unsafe impl<FA: FrameAllocator, B: MemoryBackend> Sync for PageTableManager<'_, FA, B> {}

impl<'a, FA: FrameAllocator, B: MemoryBackend> PageTableManager<'a, FA, B> {
    /// Получить размер фрейма
    pub fn frame_size(&self) -> usize {
        self.frame_size
    }

    /// Получить ссылку на backend (безопасно, так как гарантируется владельцем)
    fn backend(&self) -> &B {
        self.backend
    }

    /// Создать новый менеджер таблиц страниц с новой корневой таблицей
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
            root_table.start_address(backend.frame_size()).as_usize(),
        );

        let frame_address = root_table.start_address(backend.frame_size());
        let root_ptr: MemoryPtr<PageTable> = frame_address.into();

        // Очищаем корневую таблицу
        let mut page_table = PageTable::new();
        page_table.clear();

        root_ptr.write(backend, page_table);

        Ok(PageTableManager {
            frame_size: frame_allocator.frame_size(),
            root_table,
            frame_allocator,
            backend,
            heap,
        })
    }

    /// Включить или выключить режим виртуальной памяти
    pub fn enable_virtual_mode(&self) {
        self.backend()
            .enable_virtual_mode(self.root_table.start_address(self.frame_size));
    }

    /// Отобразить регион памяти с идентичным маппингом
    fn identity_map_region(
        &self,
        from: PageAlignedAddress,
        to: PageAlignedAddress,
        flags: EntryFlags,
    ) -> Result<(), VmError> {
        assert!(from.as_usize() < to.as_usize());

        for addr in (from.as_usize()..to.as_usize()).step_by(self.frame_size) {
            let page = Page::containing_address(VirtualAddress::new(addr), self.frame_size);
            let frame = Frame::containing_address(PhysicalAddress::new(addr), self.frame_size);
            self.map(page, frame, flags)?;
        }

        Ok(())
    }

    /// Отобразить виртуальную страницу на физический фрейм
    pub fn map(&self, page: Page, frame: Frame, flags: EntryFlags) -> Result<(), VmError> {
        let indices = page.start_address(self.frame_size).page_table_indices();
        let mut table_frame = self.root_table;

        // Проходим через уровни таблиц страниц (0-2)
        for level in 0..3 {
            let table_address = table_frame.start_address(self.frame_size);
            let mut table: PageTable = self.backend().read(table_address);
            let index = indices[level];

            if !table[index].is_valid() {
                // Создаем новую таблицу страниц
                let new_table_frame = self
                    .frame_allocator
                    .allocate_frame()
                    .ok_or(VmError::FrameAllocationFailed { level, index })?;

                // Инициализируем новую таблицу
                let new_table_address = new_table_frame.start_address(self.frame_size);
                let mut new_table = PageTable::new();
                new_table.clear();
                self.backend().write(new_table_address, new_table);

                // Связываем ее с родительской таблицей
                table[index].set(PageTableEntry::new_table(new_table_frame, self.frame_size));
                self.backend().write(table_address, table);

                table_frame = new_table_frame;
            } else if table[index].is_table() {
                // Следуем за существующей таблицей
                table_frame = table[index].frame(self.frame_size);
            } else {
                // Запись является блоковым маппингом, что конфликтует с нашим страничным маппингом
                return Err(VmError::BlockMappingExists);
            }
        }

        // Теперь мы на уровне 3 (конечная) таблица страниц
        let leaf_address = table_frame.start_address(self.frame_size);
        let mut leaf_table: PageTable = self.backend().read(leaf_address);
        let idx = indices[3];

        if leaf_table[idx].is_valid() {
            return Err(VmError::AlreadyMapped);
        }

        // Создаем финальное отображение
        leaf_table[idx].set(PageTableEntry::new_frame(frame, flags, self.frame_size));
        self.backend().write(leaf_address, leaf_table);

        let phys_addr = frame.start_address(self.frame_size);
        self.backend().clean_page_cache(phys_addr);
        self.backend().invalidate_cache();

        Ok(())
    }

    /// Размапить виртуальную страницу
    pub fn unmap(&self, page: Page) -> Result<Frame, VmError> {
        let indices = page.start_address(self.frame_size).page_table_indices();
        let mut table_frame = self.root_table;

        // Переходим к конечной таблице страниц
        for level in 0..3 {
            let table_address = table_frame.start_address(self.frame_size);
            let table: PageTable = self.backend().read(table_address);
            let idx = indices[level];

            if !table[idx].is_valid() || !table[idx].is_table() {
                return Err(VmError::NotMapped);
            }
            table_frame = table[idx].frame(self.frame_size);
        }

        // На конечном уровне (уровень 3)
        let leaf_address = table_frame.start_address(self.frame_size);
        let mut leaf_table: PageTable = self.backend().read(leaf_address);
        let idx = indices[3];

        if !leaf_table[idx].is_valid() {
            return Err(VmError::NotMapped);
        }

        if leaf_table[idx].is_table() {
            return Err(VmError::InvalidTableEntry);
        }

        // Получаем фрейм перед размапом
        let frame = leaf_table[idx].frame(self.frame_size);

        // Очищаем запись
        leaf_table[idx].clear();
        self.backend().write(leaf_address, leaf_table);

        // TODO: Рассмотреть освобождение пустых таблиц страниц для предотвращения утечек памяти

        Ok(frame)
    }

    /// Отобразить диапазон страниц на диапазон фреймов
    pub fn map_range(
        &self,
        pages: impl Iterator<Item = Page>,
        frames: impl Iterator<Item = Frame>,
        flags: EntryFlags,
    ) -> Result<(), VmError> {
        for (page, frame) in pages.zip(frames) {
            self.map(page, frame, flags)?;
        }
        Ok(())
    }

    /// Транслировать виртуальный адрес в физический адрес
    pub fn translate(&self, addr: VirtualAddress) -> Option<PhysicalAddress> {
        let indices = addr.page_table_indices();
        let mut table_frame = self.root_table;

        // Проходим через уровни таблиц страниц
        for level in 0..4 {
            let table_address = table_frame.start_address(self.frame_size);
            let table: PageTable = self.backend().read(table_address);
            let idx = indices[level];
            let entry = table[idx];

            if !entry.is_valid() {
                return None;
            }

            // Проверяем блоковые маппинги на уровнях 1 и 2
            if level > 0 && entry.is_block() {
                let block_size = match level {
                    1 => 1024 * 1024 * 1024, // 1GB
                    2 => 2 * 1024 * 1024,    // 2MB
                    _ => return None,        // Недействительно
                };

                let block_mask = block_size - 1;
                let offset = addr.as_usize() & block_mask;
                return Some(entry.address().add(offset));
            }

            // На уровне 3 должно быть страничное отображение
            if level == 3 {
                if entry.is_table() {
                    return None; // Недействительно: уровень 3 не может иметь записи таблиц
                }
                let offset = addr.page_offset();
                return Some(entry.address().add(offset));
            }

            // Продолжаем к следующему уровню
            if !entry.is_table() {
                return None; // Должна быть запись таблицы на уровнях 0-2
            }

            table_frame = entry.frame(self.frame_size);
        }

        None
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
pub(crate) fn create_page_table_manager<'a, FA: FrameAllocator, B: MemoryBackend>(
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
        page_table_manager.identity_map_region(region.start, region.end, region.flags)?;
        debug!(console(), "Direct-mapped region: {}", region.label);
    }

    info!(console(), "Direct-mapped regions initialized");

    Ok(page_table_manager)
}
