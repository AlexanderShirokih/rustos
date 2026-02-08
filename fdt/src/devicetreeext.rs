use crate::devicetree::{Node, Property};
use collections::Vec;
use core::cmp::min;

/// Содержит пару адрес (сдвиг) и размер
#[derive(Debug, Copy, Clone)]
pub struct AddressSpace {
    pub offset: usize,
    pub size: usize,
}

impl AddressSpace {
    pub fn start(&self) -> usize {
        self.offset
    }

    pub fn end(&self) -> usize {
        self.offset + self.size
    }
}

/// Адресное пространство шины
#[derive(Debug, Copy, Clone, Default)]
pub struct Ranges {
    /// Адрес устройства в координатах шины
    child: usize,

    /// Адрес в координатах родительской шины
    parent: usize,

    /// Размер адресного пространства шины
    size: usize,
}

impl Ranges {
    pub fn child_addr(&self) -> usize {
        self.child
    }

    pub fn parent_addr(&self) -> usize {
        self.parent
    }

    pub fn size(&self) -> usize {
        self.size
    }
}

/// Кратность регистров устройства в 32-битных словах
#[derive(Debug, Copy, Clone)]
pub struct CellsSize {
    /// Количество 32-битных ячеек для описания адреса
    address_cells: usize,

    /// Количество 32-битных ячеек для описания размера
    size_cells: usize,
}

impl CellsSize {
    pub fn stride(&self) -> usize {
        self.address_cells + self.size_cells
    }
}

impl Default for CellsSize {
    fn default() -> Self {
        CellsSize {
            address_cells: 2,
            size_cells: 1,
        }
    }
}

/// Дополнительные методы для работы с узлом.
pub trait NodeExt {
    /// Проверяет совместимость узла с указанным драйвером.
    fn is_compatible(&self, name: &str) -> bool;

    /// Извлекает адресное пространство из свойства `ranges`.
    fn try_get_range(&self, parent_cells_size: CellsSize) -> Option<Ranges>;

    /// Возвращает размерность ячеек адреса и размера для этого узла.
    fn cells_size(&self) -> Option<CellsSize>;
}

impl NodeExt for Node<'_> {
    fn is_compatible(&self, name: &str) -> bool {
        self.prop("compatible")
            .is_some_and(|prop| prop.into_string_list_iter().any(|s| s == name))
    }

    fn try_get_range(&self, parent_cells_size: CellsSize) -> Option<Ranges> {
        // Разметка атрибута ranges:
        // <child_addr> <parent_addr_hi> <parent_addr_lo> <size>
        // Пример:
        // ranges = < 0x00      0x10      0x00      0x80000000 >
        //         child     parent_addr   parent_addr      size
        //         addr       addr_hi addr_lo

        let ranges_prop = self.prop("ranges")?;

        let child_cells_size = self.cells_size().unwrap_or_default();

        let child_addr = ranges_prop.try_read_cell(child_cells_size.address_cells, 0)?;

        let parent_addr = ranges_prop.try_read_cell(
            parent_cells_size.address_cells,
            child_cells_size.address_cells,
        )?;

        let size = ranges_prop.try_read_cell(
            child_cells_size.size_cells,
            child_cells_size.address_cells + parent_cells_size.address_cells,
        )?;

        Some(Ranges {
            child: child_addr as usize,
            parent: parent_addr as usize,
            size: size as usize,
        })
    }

    fn cells_size(&self) -> Option<CellsSize> {
        let address_cells = self
            .prop("#address-cells")
            .map_or(2, |prop| prop.as_usize());

        let size_cells = self.prop("#size-cells").map_or(1, |prop| prop.as_usize());

        Some(CellsSize {
            address_cells,
            size_cells,
        })
    }
}

/// Дополнительные методы для работы со свойством.
pub trait PropExt<'a> {
    /// Извлекает список регистров (адрес + размер) из свойства `reg`.
    fn try_as_reg_list<const N: usize>(
        &self,
        cells_size: CellsSize,
    ) -> Option<Vec<AddressSpace, N>>;

    /// Возвращает итератор по строкам (например, для `compatible`).
    fn into_string_list_iter(self) -> impl Iterator<Item = &'a str> + 'a;
}

impl Property<'_> {
    fn try_read_cell(&self, cells: usize, offset: usize) -> Option<u64> {
        let mut value = 0u64;

        for cell in 0..cells {
            let reg = self.try_as_u32((offset + cell) * size_of::<u32>())? as u64;
            value = (value << 32) | reg;
        }

        Some(value)
    }
}

impl<'a> PropExt<'a> for Property<'a> {
    fn try_as_reg_list<const N: usize>(
        &self,
        cells_size: CellsSize,
    ) -> Option<Vec<AddressSpace, N>> {
        let stride_cells = cells_size.stride();
        let total_cells = self.value().len() / size_of::<u32>();

        if stride_cells == 0 || total_cells < stride_cells {
            return None;
        }

        let count = min(N, total_cells / stride_cells);

        let address_cells = cells_size.address_cells;
        let size_cells = cells_size.size_cells;

        let mut result = Vec::new();

        for i in 0..count {
            let base = i * stride_cells;
            let address = self.try_read_cell(address_cells, base)?;
            let size = self.try_read_cell(size_cells, base + address_cells)?;

            result.push(AddressSpace {
                offset: address as usize,
                size: size as usize,
            });
        }

        Some(result)
    }

    fn into_string_list_iter(self) -> impl Iterator<Item = &'a str> + 'a {
        self.value()
            .split(|&b| b == 0)
            .filter(|s| !s.is_empty())
            .filter_map(|s| str::from_utf8(s).ok())
    }
}
