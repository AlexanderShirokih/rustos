use drivers_common::probe::ProbeContext;
use drivers_common::{DeviceNode, NodeProperty};
use drivers_common_aarch64::ProbeContextExt;
use drivers_common_aarch64::tree_ext::{AddressSpace, BusRange, CellsSize, NodeAddressExt};

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

    fn prop(&self, name: &str) -> Option<Self::Property<'_>> {
        self.props
            .iter()
            .find(|(n, _)| *n == name)
            .map(|(n, raw)| TestProp { name: n, raw })
    }

    fn children(&self) -> Self::ChildIter<'_> {
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

    fn reg_iter(&self, _cells_size: CellsSize) -> impl Iterator<Item = AddressSpace> {
        self.regs.iter().copied()
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

    let first_reg = uart.reg_iter(soc.cells_size().unwrap()).next().unwrap();

    let range = soc
        .range_to_parent(CellsSize {
            address_cells: 2,
            size_cells: 1,
        })
        .unwrap();

    let translated = range.parent + first_reg.offset - range.child;

    assert_eq!(translated, 0x1000_2000);
}

#[test]
fn probe_context_ext_translates_reg_address_space() {
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
    let soc_children = [uart];
    let soc = TestNode {
        id: 2,
        name: "soc",
        props: &[],
        children: &soc_children,
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
    let root_children = [soc];
    let root = TestNode {
        id: 1,
        name: "",
        props: &[],
        children: &root_children,
        cells: Some(CellsSize {
            address_cells: 2,
            size_cells: 1,
        }),
        range: None,
        regs: &[],
    };

    let context = ProbeContext::new(uart, vec![root, soc, uart]);
    let reg = context.get_address(0);

    assert_eq!(reg.offset, 0x1000_2000);
    assert_eq!(reg.size, 0x1000);
}
