use crate::virtual_address::AlignedVirtualAddress;

/// Трейт для низкоуровневых операций с кэшем данных. Реализация зависит от архитектуры:
/// набор кэш-инструкций у платформ разный.
pub trait MemoryAccessProvider {
    /// Пишет данные по адресу `address` из кэша в память, не инвалидируя строку.
    fn clean_cache<const SHIFT: u8>(&self, address: AlignedVirtualAddress<SHIFT>);

    /// Инвалидирует весь кэш.
    fn invalidate_cache(&self);
}
