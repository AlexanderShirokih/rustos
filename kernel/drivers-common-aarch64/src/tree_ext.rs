//! Расширения дерева устройств для FDT-адресации.

use drivers_common::{DeviceNode, services::mmio::MmioAddress};

/// Размерность адресных ячеек в узле.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CellsSize {
    /// Количество 32-битных ячеек адреса.
    pub address_cells: usize,
    /// Количество 32-битных ячеек размера.
    pub size_cells: usize,
}

impl CellsSize {
    /// Возвращает суммарный stride в 32-битных ячейках.
    pub const fn stride(&self) -> usize {
        self.address_cells + self.size_cells
    }
}

impl Default for CellsSize {
    fn default() -> Self {
        Self {
            address_cells: 2,
            size_cells: 1,
        }
    }
}

/// Диапазон адресного пространства устройства.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AddressSpace {
    /// Смещение (или базовый адрес) диапазона.
    pub offset: usize,
    /// Размер диапазона в байтах.
    pub size: usize,
}

impl AddressSpace {
    pub const fn as_mmio(&self) -> Option<MmioAddress> {
        MmioAddress::new(self.offset, self.size)
    }
}

/// Адресные расширения поверх узла (для MMIO-инициализации драйверов).
pub trait NodeAddressExt: DeviceNode {
    /// Читает `#address-cells`/`#size-cells` узла и возвращает их размерность.
    fn cells_size(&self) -> Option<CellsSize>;
    /// Читает `reg` как список адресных диапазонов.
    fn reg_iter(&self, cells_size: CellsSize) -> impl Iterator<Item = AddressSpace>;
}
