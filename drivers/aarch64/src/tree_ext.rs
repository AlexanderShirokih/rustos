//! Расширения дерева устройств для FDT-адресации.

use alloc::vec::Vec;
use drivers_common::DeviceNode;

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

/// Трансляция child -> parent для адресного пространства шины.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Default)]
pub struct BusRange {
    /// Адрес в координатах дочерней шины.
    pub child: usize,
    /// Адрес в координатах родительской шины.
    pub parent: usize,
    /// Размер диапазона.
    pub size: usize,
}

/// Адресные расширения поверх узла (для MMIO-инициализации драйверов).
pub trait NodeAddressExt: DeviceNode {
    /// Возвращает размерность ячеек этого узла.
    fn cells_size(&self) -> Option<CellsSize>;
    /// Читает `ranges` и возвращает трансляцию child -> parent.
    fn range_to_parent(&self, parent_cells: CellsSize) -> Option<BusRange>;
    /// Читает `reg` как список адресных диапазонов.
    fn reg_list(&self, cells_size: CellsSize, limit: usize) -> Vec<AddressSpace>;
}
