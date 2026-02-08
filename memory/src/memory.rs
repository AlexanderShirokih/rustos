use crate::virtual_address::AlignedVirtualAddress;

/// Трейт для низкоуровневых операций с памятью.
pub trait MemoryAccessProvider {
    /// Очищает кэш для указанного адреса.
    fn clean_cache<const SHIFT: u8>(&self, address: AlignedVirtualAddress<SHIFT>);

    /// Инвалидирует весь кэш.
    fn invalidate_cache(&self);
}
