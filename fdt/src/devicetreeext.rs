use crate::devicetree::{DeviceTree, Node, Property};

/// Содержит пару адрес (сдвиг) и размер
pub struct OffsetSize {
    pub offset: usize,
    pub size: usize,
}

impl OffsetSize {
    pub fn start(&self) -> usize {
        self.offset
    }

    pub fn end(&self) -> usize {
        self.offset + self.size
    }
}

#[derive(Copy, Clone)]
pub struct CellsSize {
    address_cells: usize,
    size_cells: usize,
}

impl CellsSize {
    pub fn stride(&self) -> usize {
        (self.address_cells + self.size_cells) * size_of::<u32>()
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

pub trait DeviceTreeExt {
    fn cells_size(&self) -> Option<CellsSize>;
}

impl DeviceTreeExt for DeviceTree<'_> {
    fn cells_size(&self) -> Option<CellsSize> {
        let root = self.root()?;

        let address_cells = root
            .prop("#address-cells")
            .map_or(2, |prop| prop.as_usize());

        let size_cells = root.prop("#size-cells").map_or(1, |prop| prop.as_usize());

        Some(CellsSize {
            address_cells,
            size_cells,
        })
    }
}

pub trait NodeExt {
    fn is_compatible(&self, name: &str) -> bool;
}

impl NodeExt for Node<'_> {
    fn is_compatible(&self, name: &str) -> bool {
        self.prop("compatible").map_or(false, |prop| {
            prop.into_string_list_iter().any(|s| s == name)
        })
    }
}

pub trait PropExt<'a> {
    fn try_as_offset_size(&self, cells_size: CellsSize) -> Option<OffsetSize>;

    fn into_string_list_iter(self) -> impl Iterator<Item = &'a str> + 'a;
}

impl<'a> PropExt<'a> for Property<'a> {
    fn try_as_offset_size(&self, cells_size: CellsSize) -> Option<OffsetSize> {
        let stride = cells_size.stride();

        if stride == 0 || self.value().len() < stride {
            return None;
        }

        let address_cells = cells_size.address_cells;
        let size_cells = cells_size.size_cells;

        let mut offset = 0usize;

        let mut address = 0usize;
        for _ in 0..address_cells {
            let value = self.try_as_u32(offset)? as usize;
            offset += size_of::<u32>();
            address = (address << 32) | value;
        }

        let mut size = 0usize;
        for cell in 0..size_cells {
            let offset = (address_cells + cell) * size_of::<u32>();
            let value = self.try_as_u32(offset)? as usize;
            size = (size << 32) | value;
        }

        Some(OffsetSize {
            offset: address,
            size,
        })
    }

    fn into_string_list_iter(self) -> impl Iterator<Item = &'a str> + 'a {
        self.value()
            .split(|&b| b == 0)
            .filter(|s| !s.is_empty())
            .filter_map(|s| str::from_utf8(s).ok())
    }
}
