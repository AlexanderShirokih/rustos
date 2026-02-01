use crate::level::Level;
use crate::page_table::PageTable;
use memory::physical_address::PageAlignedAddress;

pub trait TableAlloc {
    /// Выделить физическую страницу 4K под page table и вернуть её PA.
    fn alloc_table_page(&mut self) -> Option<PageAlignedAddress>;

    /// Получить указатель на page table уровня L.
    ///
    /// # Arguments
    /// * `pa` - физический адрес page table
    /// * `target_va` - целевой VA, который обслуживает эта таблица (для recursive mapping)
    /// # Safety
    /// - Caller гарантирует, что page table существует
    /// - `target_va` должен быть canonical address
    unsafe fn table_ptr<L: Level>(
        &self,
        pa: PageAlignedAddress,
        target_va: usize,
    ) -> *mut PageTable<L>;
}
