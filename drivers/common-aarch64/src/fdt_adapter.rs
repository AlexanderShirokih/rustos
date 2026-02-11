extern crate alloc;

use crate::tree_ext::{AddressSpace, BusRange, CellsSize, NodeAddressExt};
use core::mem::size_of;
use drivers_common::{DeviceNode, DeviceTreeSource, NodeProperty};
use fdt::devicetree::{DeviceTree, Node, NodeIter, NodeKey, Property};

#[derive(Clone, Copy)]
pub struct FdtTree<'a> {
    inner: &'a DeviceTree<'a>,
}

#[derive(Clone, Copy)]
pub struct FdtNode<'a> {
    inner: Node<'a>,
}

#[derive(Clone, Copy)]
pub struct FdtProperty<'a> {
    inner: Property<'a>,
}

pub struct FdtNodeIter<'a> {
    inner: NodeIter<'a>,
}

pub struct RegIter<'a> {
    bytes: &'a [u8],
    cells_size: CellsSize,
    index: usize,
    count: usize,
}

/// Обёртка над `DeviceTree` для передачи в драйверный слой.
pub fn adapt_tree<'a>(device_tree: &'a DeviceTree<'a>) -> FdtTree<'a> {
    FdtTree { inner: device_tree }
}

impl<'dt> FdtTree<'dt> {
    /// Возвращает корневой узел.
    pub fn root(&self) -> Option<FdtNode<'dt>> {
        self.inner.root().map(FdtNode::new)
    }
}

impl<'dt> FdtNode<'dt> {
    pub(crate) const fn new(node: Node<'dt>) -> FdtNode<'dt> {
        FdtNode { inner: node }
    }
}

impl<'dt> Iterator for FdtNodeIter<'dt> {
    type Item = FdtNode<'dt>;

    fn next(&mut self) -> Option<Self::Item> {
        self.inner.next().map(FdtNode::new)
    }
}

impl<'dt> NodeProperty for FdtProperty<'dt> {
    fn name(&self) -> &str {
        self.inner.name()
    }

    fn raw(&self) -> &[u8] {
        self.inner.value()
    }

    fn as_cstr(&self) -> Option<&str> {
        self.inner.as_cstr()
    }
}

impl<'dt> DeviceNode for FdtNode<'dt> {
    type Id = NodeKey;
    type Property<'a>
        = FdtProperty<'dt>
    where
        Self: 'a;
    type ChildIter<'a>
        = FdtNodeIter<'dt>
    where
        Self: 'a;

    fn id(&self) -> Self::Id {
        self.inner.key()
    }

    fn name(&self) -> &str {
        self.inner.name()
    }

    fn prop(&self, name: &str) -> Option<Self::Property<'_>> {
        self.inner.prop(name).map(|inner| FdtProperty { inner })
    }

    fn children(&self) -> Self::ChildIter<'_> {
        FdtNodeIter {
            inner: self.inner.children(),
        }
    }
}

impl<'dt> DeviceTreeSource for FdtTree<'dt> {
    type Node<'a>
        = FdtNode<'dt>
    where
        Self: 'a;
    type NodeIter<'a>
        = FdtNodeIter<'dt>
    where
        Self: 'a;

    fn root(&self) -> Option<Self::Node<'_>> {
        self.root()
    }

    fn nodes(&self) -> Self::NodeIter<'_> {
        FdtNodeIter {
            inner: self.inner.nodes(),
        }
    }
}

impl<'dt> NodeAddressExt for FdtNode<'dt> {
    fn cells_size(&self) -> Option<CellsSize> {
        let address_cells = self
            .inner
            .prop("#address-cells")
            .map_or(2, |prop| prop.as_usize());
        let size_cells = self
            .inner
            .prop("#size-cells")
            .map_or(1, |prop| prop.as_usize());

        Some(CellsSize {
            address_cells,
            size_cells,
        })
    }

    fn range_to_parent(&self, parent_cells: CellsSize) -> Option<BusRange> {
        let ranges_prop = self.inner.prop("ranges")?;
        let child_cells = self.cells_size().unwrap_or_default();
        let child = try_read_cell(ranges_prop.value(), child_cells.address_cells, 0)?;
        let parent = try_read_cell(
            ranges_prop.value(),
            parent_cells.address_cells,
            child_cells.address_cells,
        )?;
        let size = try_read_cell(
            ranges_prop.value(),
            child_cells.size_cells,
            child_cells.address_cells + parent_cells.address_cells,
        )?;

        Some(BusRange {
            child: child as usize,
            parent: parent as usize,
            size: size as usize,
        })
    }

    fn reg_iter(&self, cells_size: CellsSize) -> impl Iterator<Item = AddressSpace> {
        let Some(reg) = self.inner.prop("reg") else {
            return RegIter::empty();
        };

        let stride_cells = cells_size.stride();
        if stride_cells == 0 {
            return RegIter::empty();
        }

        let total_cells = reg.value().len() / size_of::<u32>();
        if total_cells < stride_cells {
            return RegIter::empty();
        }

        let count = total_cells / stride_cells;
        RegIter::new(reg.value(), cells_size, count)
    }
}

impl<'a> RegIter<'a> {
    fn new(bytes: &'a [u8], cells_size: CellsSize, count: usize) -> Self {
        Self {
            bytes,
            cells_size,
            index: 0,
            count,
        }
    }

    fn empty() -> Self {
        Self::new(&[], CellsSize::default(), 0)
    }
}

impl<'a> Iterator for RegIter<'a> {
    type Item = AddressSpace;

    fn next(&mut self) -> Option<Self::Item> {
        let stride_cells = self.cells_size.stride();
        while self.index < self.count {
            let base = self.index * stride_cells;
            self.index += 1;

            let Some(address) = try_read_cell(self.bytes, self.cells_size.address_cells, base)
            else {
                continue;
            };

            let Some(size) = try_read_cell(
                self.bytes,
                self.cells_size.size_cells,
                base + self.cells_size.address_cells,
            ) else {
                continue;
            };

            return Some(AddressSpace {
                offset: address as usize,
                size: size as usize,
            });
        }

        None
    }
}

fn try_read_cell(bytes: &[u8], cells: usize, offset_cells: usize) -> Option<u64> {
    let mut value = 0u64;

    for cell in 0..cells {
        let offset = (offset_cells + cell) * size_of::<u32>();
        let slice = bytes.get(offset..offset + size_of::<u32>())?;
        let reg = u32::from_be_bytes(slice.try_into().ok()?) as u64;
        value = (value << 32) | reg;
    }

    Some(value)
}
