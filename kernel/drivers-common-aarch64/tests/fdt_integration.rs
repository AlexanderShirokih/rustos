//! Интеграционные тесты адаптера через реальный `FdtTree`/`FdtNode`,
//! построенный из настоящего DTB-blob.

#![allow(clippy::unwrap_used)]
#![allow(clippy::panic)]

use std::collections::BTreeSet;

use drivers_common::{DeviceNode, probe::ProbeContext};
use drivers_common_aarch64::{
    ProbeContextExt, adapt_to_fdt_tree,
    device_windows::enumerate_device_windows,
    fdt_adapter::FdtNode,
    is_compatible, require_compatible,
    tree_ext::{AddressSpace, CellsSize, NodeAddressExt},
};
use fdt::devicetree::DeviceTree;

const SOC_TREE: &[fdt_test::Token<'static>] = &fdt_test::tree! {
    #address-cells = <2>;
    #size-cells = <1>;
    soc {
        #address-cells = <1>;
        #size-cells = <1>;
        ranges = <0x0 0x0 0x1000_0000 0x10_0000>;
        serial@2000 {
            compatible = "arm,pl011", "arm,primecell";
            reg = <0x2000 0x1000>;
        };
        intc@3000 {
            compatible = "vendor,intc";
            reg = <0x3000 0x100 0x4000 0x1000>;
        };
        bus@5000 {
            #address-cells = <1>;
            #size-cells = <1>;
            dev@0 {
                compatible = "vendor,dev";
                reg = <0x0 0x10>;
            };
        };
    };
};

fn find_child<'a>(node: &FdtNode<'a>, name: &str) -> FdtNode<'a> {
    node.children()
        .find(|c| c.name() == name)
        .unwrap_or_else(|| panic!("node {name} is not found"))
}

// Тесты через реальный адаптер

#[test]
fn real_fdt_reg_iter_reads_reg_property() {
    let dtb = fdt_test::build(SOC_TREE);
    let dt = DeviceTree::from_bytes(&dtb).unwrap();
    let tree = adapt_to_fdt_tree(&dt);
    let root = tree.root().unwrap();
    let soc = find_child(&root, "soc");
    let serial = find_child(&soc, "serial@2000");

    // soc имеет #address-cells=1, #size-cells=1.
    let cells = soc.cells_size().unwrap();
    assert_eq!(
        cells,
        CellsSize {
            address_cells: 1,
            size_cells: 1
        }
    );

    let regs: Vec<_> = serial.reg_iter(cells).collect();
    assert_eq!(regs.len(), 1);
    assert_eq!(regs[0].offset, 0x2000);
    assert_eq!(regs[0].size, 0x1000);
}

#[test]
fn real_fdt_is_compatible_matches_string_list() {
    let dtb = fdt_test::build(SOC_TREE);
    let dt = DeviceTree::from_bytes(&dtb).unwrap();
    let tree = adapt_to_fdt_tree(&dt);
    let root = tree.root().unwrap();
    let soc = find_child(&root, "soc");
    let serial = find_child(&soc, "serial@2000");

    assert!(is_compatible(&serial, &["arm,pl011"]));
    assert!(is_compatible(&serial, &["arm,primecell"]));
    assert!(is_compatible(&serial, &["nomatch", "arm,pl011"]));
    assert!(!is_compatible(&serial, &["nomatch"]));
    // Узел без compatible.
    assert!(!is_compatible(&soc, &["arm,pl011"]));

    assert!(require_compatible(&serial, &["arm,pl011"]).is_ok());
    assert!(require_compatible(&serial, &["nomatch"]).is_err());
}

#[test]
fn real_fdt_get_address_translates_through_bus() {
    let dtb = fdt_test::build(SOC_TREE);
    let dt = DeviceTree::from_bytes(&dtb).unwrap();
    let tree = adapt_to_fdt_tree(&dt);
    let root = tree.root().unwrap();
    let soc = find_child(&root, "soc");
    let serial = find_child(&soc, "serial@2000");

    let ctx = ProbeContext::new(serial, vec![root, soc, serial], None);

    assert_eq!(
        ctx.get_address(0),
        Some(AddressSpace {
            offset: 0x1000_2000,
            size: 0x1000,
        })
    );
    assert_eq!(ctx.get_mmio_address(0).unwrap().base(), 0x1000_2000);
}

#[test]
fn real_fdt_get_address_reg_index_out_of_bounds() {
    let dtb = fdt_test::build(SOC_TREE);
    let dt = DeviceTree::from_bytes(&dtb).unwrap();
    let tree = adapt_to_fdt_tree(&dt);
    let root = tree.root().unwrap();
    let soc = find_child(&root, "soc");
    let serial = find_child(&soc, "serial@2000");

    let ctx = ProbeContext::new(serial, vec![root, soc, serial], None);

    assert_eq!(ctx.get_address(5), None);
}

#[test]
fn real_fdt_get_address_without_parent_bus() {
    let dtb = fdt_test::build(SOC_TREE);
    let dt = DeviceTree::from_bytes(&dtb).unwrap();
    let tree = adapt_to_fdt_tree(&dt);
    let root = tree.root().unwrap();
    let soc = find_child(&root, "soc");

    let ctx = ProbeContext::new(soc, vec![soc], None);

    assert_eq!(ctx.get_address(0), None);
}

#[test]
fn real_fdt_get_address_reads_each_reg_entry() {
    let dtb = fdt_test::build(SOC_TREE);
    let dt = DeviceTree::from_bytes(&dtb).unwrap();
    let root = adapt_to_fdt_tree(&dt).root().unwrap();
    let soc = find_child(&root, "soc");
    let intc = find_child(&soc, "intc@3000");

    let ctx = ProbeContext::new(intc, vec![root, soc, intc], None);

    assert_eq!(
        ctx.get_address(1),
        Some(AddressSpace {
            offset: 0x1000_4000,
            size: 0x1000,
        })
    );
}

#[test]
fn real_fdt_get_address_behind_a_bus_without_ranges_is_none() {
    let dtb = fdt_test::build(SOC_TREE);
    let dt = DeviceTree::from_bytes(&dtb).unwrap();
    let root = adapt_to_fdt_tree(&dt).root().unwrap();
    let soc = find_child(&root, "soc");
    let bus = find_child(&soc, "bus@5000");
    let dev = find_child(&bus, "dev@0");

    let ctx = ProbeContext::new(dev, vec![root, soc, bus, dev], None);

    assert_eq!(ctx.get_address(0), None);
}

#[test]
fn a_claimed_node_gives_marked_windows() {
    let dtb = fdt_test::build(SOC_TREE);
    let dt = DeviceTree::from_bytes(&dtb).unwrap();
    let root = adapt_to_fdt_tree(&dt).root().unwrap();
    let intc = find_child(&find_child(&root, "soc"), "intc@3000");
    let windows = |claimed: &BTreeSet<usize>| -> Vec<(usize, usize, bool)> {
        enumerate_device_windows(&root, claimed)
            .iter()
            .map(|window| (window.window.offset, window.window.size, window.excluded))
            .collect()
    };

    assert_eq!(
        windows(&BTreeSet::new()),
        [
            (0x1000_2000, 0x1000, false),
            (0x1000_3000, 0x100, false),
            (0x1000_4000, 0x1000, false)
        ]
    );
    assert_eq!(
        windows(&BTreeSet::from([intc.id()])),
        [
            (0x1000_2000, 0x1000, false),
            (0x1000_3000, 0x100, true),
            (0x1000_4000, 0x1000, true)
        ]
    );
}
