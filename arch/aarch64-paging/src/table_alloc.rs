//! Аллокатор таблиц страниц.

use crate::level::Level;
use crate::page_table::PageTable;
use memory::physical_address::PageAlignedAddress;

/// Аллокатор таблиц страниц.
///
/// Отвечает за выделение памяти под новые таблицы и получение
/// указателей на них по физическому адресу.
pub trait TableAlloc {
    /// Выделяет физическую страницу 4 КБ под таблицу.
    fn alloc_table_page(&mut self) -> Option<PageAlignedAddress>;

    /// Возвращает указатель на таблицу по её физическому адресу.
    ///
    /// # Safety
    /// Таблица по адресу `pa` должна существовать и быть валидной.
    unsafe fn table_ptr<L: Level>(
        &self,
        pa: PageAlignedAddress,
        target_va: usize,
    ) -> *mut PageTable<L>;
}
