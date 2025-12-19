use crate::level::{Level, PagePa};
use crate::page_table::PageTable;

pub trait TableAlloc {
    /// Выделить физическую страницу 4K под page table и вернуть её PA.
    fn alloc_table_page(&mut self) -> Option<PagePa>;

    /// Получить указатель на PageTable<L> по физическому адресу таблицы.
    unsafe fn table_ptr<L: Level>(&self, pa: &PagePa) -> *mut PageTable<L>;
}
