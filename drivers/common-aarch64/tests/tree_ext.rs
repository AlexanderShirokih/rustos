use drivers_common_aarch64::tree_ext::{AddressSpace, BusRange, CellsSize, NodeAddressExt};
use drivers_common::{DeviceNode, NodeProperty};
use std::vec::Vec;

#[derive(Clone, Copy, Debug)]
struct TestProp<'a> {
    name: &'a str,
    raw: &'a [u8],
}

impl NodeProperty for TestProp<'_> {
    fn name(&self) -> &str {
        self.name
    }

    fn raw(&self) -> &[u8] {
        self.raw
    }

    fn as_cstr(&self) -> Option<&str> {
        let len = self.raw.len().saturating_sub(1);
        core::str::from_utf8(&self.raw[..len]).ok()
    }
}

#[derive(Clone, Copy)]
struct TestNode<'a> {
    id: usize,
    name: &'a str,
    props: &'a [(&'a str, &'a [u8])],
    children: &'a [TestNode<'a>],
    cells: Option<CellsSize>,
    range: Option<BusRange>,
    regs: &'a [AddressSpace],
}

impl DeviceNode for TestNode<'_> {
    type Id = usize;
    type Property<'a>
        = TestProp<'a>
    where
        Self: 'a;
    type ChildIter<'a>
        = core::iter::Copied<core::slice::Iter<'a, Self>>
    where
        Self: 'a;

    fn id(&self) -> Self::Id {
        self.id
    }

    fn name(&self) -> &str {
        self.name
    }

    fn prop<'a>(&'a self, name: &str) -> Option<Self::Property<'a>> {
        self.props
            .iter()
            .find(|(n, _)| *n == name)
            .map(|(n, raw)| TestProp { name: n, raw })
    }

    fn children<'a>(&'a self) -> Self::ChildIter<'a> {
        self.children.iter().copied()
    }
}

impl NodeAddressExt for TestNode<'_> {
    fn cells_size(&self) -> Option<CellsSize> {
        self.cells
    }

    fn range_to_parent(&self, _parent_cells: CellsSize) -> Option<BusRange> {
        self.range
    }

    fn reg_list(&self, _cells_size: CellsSize, limit: usize) -> Vec<AddressSpace> {
        self.regs.iter().copied().take(limit).collect()
    }
}

#[test]
fn node_address_ext_works_for_mmio_translation() {
    let uart = TestNode {
        id: 3,
        name: "serial@2000",
        props: &[],
        children: &[],
        cells: None,
        range: None,
        regs: &[AddressSpace {
            offset: 0x2000,
            size: 0x1000,
        }],
    };
    let soc = TestNode {
        id: 2,
        name: "soc",
        props: &[],
        children: &[uart],
        cells: Some(CellsSize {
            address_cells: 1,
            size_cells: 1,
        }),
        range: Some(BusRange {
            child: 0,
            parent: 0x1000_0000,
            size: 0x0010_0000,
        }),
        regs: &[],
    };

    let regs = uart.reg_list(soc.cells_size().unwrap(), 8);
    let range = soc
        .range_to_parent(CellsSize {
            address_cells: 2,
            size_cells: 1,
        })
        .unwrap();
    let translated = range.parent + regs[0].offset - range.child;

    assert_eq!(translated, 0x1000_2000);
}
