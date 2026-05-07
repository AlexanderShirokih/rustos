//! Общие моки и хелперы для интеграционных тестов hal-aarch64-paging.
#![allow(unsafe_code)]

use std::sync::atomic::{AtomicUsize, Ordering};

use hal_aarch64_paging::{
    level::{L0, Level},
    mapper::PageMapper,
    page_table::PageTable,
    table_alloc::TableAlloc,
};
use memory::{aligned::Aligned, physical_address::PageAlignedAddress};

const PAGE_SIZE: usize = PageAlignedAddress::ALIGNMENT;

/// Минимальный `TableAlloc` для тестов: выделяет страницы из `'static`-буфера.
/// PA = индекс страницы * 4 КБ; реальный VA = `base + PA` (как higher-half mapping).
pub struct MockAlloc {
    base: usize,
    capacity: usize,
    next: AtomicUsize,
}

impl MockAlloc {
    pub fn new(buffer: &'static [u8]) -> Self {
        let raw = buffer.as_ptr() as usize;
        let base = (raw + PAGE_SIZE - 1) & !(PAGE_SIZE - 1);
        let usable = buffer.len() - (base - raw);
        Self {
            base,
            capacity: usable / PAGE_SIZE,
            next: AtomicUsize::new(0),
        }
    }

    pub fn base(&self) -> usize {
        self.base
    }
}

impl TableAlloc for MockAlloc {
    fn alloc_table_page(&mut self) -> Option<PageAlignedAddress> {
        let idx = self.next.fetch_add(1, Ordering::Relaxed);
        if idx >= self.capacity {
            self.next.fetch_sub(1, Ordering::Relaxed);
            return None;
        }
        PageAlignedAddress::from_usize(idx * PAGE_SIZE)
    }

    unsafe fn table_ptr<L: Level>(
        &self,
        pa: PageAlignedAddress,
        _target_va: usize,
    ) -> *mut PageTable<L> {
        (self.base + pa.as_usize()) as *mut PageTable<L>
    }
}

/// Заглушка `Box::leak(Vec<u8>)` нужного размера. `pages + 1` ради padding'а
/// под выравнивание начала.
pub fn allocate_buffer(pages: usize) -> &'static [u8] {
    let buf = vec![0u8; pages * PAGE_SIZE + PAGE_SIZE].into_boxed_slice();
    Box::leak(buf)
}

/// Создаёт `PageMapper` с root L0 в первой странице буфера и алло́ком,
/// раздающим оставшиеся страницы. Возвращает `(mapper, base)` - `base`
/// нужен тестам, проверяющим VA-преобразование.
pub fn make_mapper(buffer: &'static [u8]) -> (PageMapper<MockAlloc>, usize) {
    let mut alloc = MockAlloc::new(buffer);
    let base = alloc.base();
    let root_pa = alloc.alloc_table_page().expect("root table alloc");
    let root_ptr = (base + root_pa.as_usize()) as *mut PageTable<L0>;
    // SAFETY: страница занулена через `Vec<u8>`, эксклюзивно владеется тестом.
    unsafe { root_ptr.write(PageTable::<L0>::new()) };
    (PageMapper::new(root_ptr, alloc), base)
}
