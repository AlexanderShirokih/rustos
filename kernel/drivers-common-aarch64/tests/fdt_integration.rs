//! Интеграционные тесты адаптера через реальный `FdtTree`/`FdtNode`,
//! построенный из настоящего DTB-blob.
//!
//! Это исполняет реальные реализации `RegIter`, `range_to_parent`,
//! `try_read_cell`, `is_compatible` и трансляцию адресов в `get_address`.

use drivers_common::{DeviceNode, probe::ProbeContext};
use drivers_common_aarch64::{
    ProbeContextExt, adapt_to_fdt_tree,
    fdt_adapter::FdtNode,
    is_compatible, require_compatible,
    tree_ext::{CellsSize, NodeAddressExt},
};
use fdt::devicetree::DeviceTree;

// DTB-билдер

fn push_u32(buf: &mut Vec<u8>, val: u32) {
    buf.extend_from_slice(&val.to_be_bytes());
}

fn align4(buf: &mut Vec<u8>) {
    while !buf.len().is_multiple_of(4) {
        buf.push(0);
    }
}

struct Strings {
    bytes: Vec<u8>,
}

impl Strings {
    fn new() -> Self {
        Self { bytes: Vec::new() }
    }

    fn intern(&mut self, name: &str) -> u32 {
        let off = self.bytes.len() as u32;
        self.bytes.extend_from_slice(name.as_bytes());
        self.bytes.push(0);
        off
    }
}

fn push_prop_u32(buf: &mut Vec<u8>, name_off: u32, val: u32) {
    push_u32(buf, 0x3);
    push_u32(buf, 4);
    push_u32(buf, name_off);
    push_u32(buf, val);
}

fn push_prop_bytes(buf: &mut Vec<u8>, name_off: u32, val: &[u8]) {
    push_u32(buf, 0x3);
    push_u32(buf, val.len() as u32);
    push_u32(buf, name_off);
    buf.extend_from_slice(val);
    align4(buf);
}

fn write_dtb(structure: &[u8], strings: &[u8]) -> Vec<u8> {
    let off_rsvmap: u32 = 40;
    let off_struct = off_rsvmap + 16;
    let off_strings = off_struct + structure.len() as u32;
    let total = off_strings + strings.len() as u32;

    let mut buf = Vec::new();
    push_u32(&mut buf, 0xD00D_FEED);
    push_u32(&mut buf, total);
    push_u32(&mut buf, off_struct);
    push_u32(&mut buf, off_strings);
    push_u32(&mut buf, off_rsvmap);
    push_u32(&mut buf, 17);
    push_u32(&mut buf, 16);
    push_u32(&mut buf, 0);
    push_u32(&mut buf, strings.len() as u32);
    push_u32(&mut buf, structure.len() as u32);
    buf.extend_from_slice(&[0u8; 16]);
    buf.extend_from_slice(structure);
    buf.extend_from_slice(strings);
    buf
}

/// ```text
/// / {
///     #address-cells = <2>; #size-cells = <1>;
///     soc {
///         #address-cells = <1>; #size-cells = <1>;
///         ranges = <0x0  0x0 0x1000_0000  0x0010_0000>;
///         serial@2000 {
///             compatible = "arm,pl011\0arm,primecell";
///             reg = <0x2000 0x1000>;
///         };
///     };
/// };
/// ```
fn build_soc_dtb() -> Vec<u8> {
    let mut s = Strings::new();
    let s_addr = s.intern("#address-cells");
    let s_size = s.intern("#size-cells");
    let s_ranges = s.intern("ranges");
    let s_compat = s.intern("compatible");
    let s_reg = s.intern("reg");

    let mut st = Vec::new();
    push_u32(&mut st, 0x1); // begin root
    push_u32(&mut st, 0x0);
    push_prop_u32(&mut st, s_addr, 2);
    push_prop_u32(&mut st, s_size, 1);

    // soc
    push_u32(&mut st, 0x1);
    st.extend_from_slice(b"soc\0");
    align4(&mut st);
    push_prop_u32(&mut st, s_addr, 1);
    push_prop_u32(&mut st, s_size, 1);
    // ranges: child(1) parent(2) size(1)
    let mut ranges = Vec::new();
    push_u32(&mut ranges, 0x0); // child
    push_u32(&mut ranges, 0x0); // parent hi
    push_u32(&mut ranges, 0x1000_0000); // parent lo
    push_u32(&mut ranges, 0x0010_0000); // size
    push_prop_bytes(&mut st, s_ranges, &ranges);

    // serial@2000
    push_u32(&mut st, 0x1);
    st.extend_from_slice(b"serial@2000\0");
    align4(&mut st);
    push_prop_bytes(&mut st, s_compat, b"arm,pl011\0arm,primecell\0");
    let mut reg = Vec::new();
    push_u32(&mut reg, 0x2000);
    push_u32(&mut reg, 0x1000);
    push_prop_bytes(&mut st, s_reg, &reg);
    push_u32(&mut st, 0x2); // end serial

    push_u32(&mut st, 0x2); // end soc
    push_u32(&mut st, 0x2); // end root
    push_u32(&mut st, 0x9); // end

    write_dtb(&st, &s.bytes)
}

fn find_child<'a>(node: &FdtNode<'a>, name: &str) -> FdtNode<'a> {
    node.children()
        .find(|c| c.name() == name)
        .unwrap_or_else(|| panic!("узел {name} не найден"))
}

// Тесты через реальный адаптер

#[test]
fn real_fdt_reg_iter_reads_reg_property() {
    let dtb = build_soc_dtb();
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
fn real_fdt_range_to_parent_reads_ranges() {
    let dtb = build_soc_dtb();
    let dt = DeviceTree::from_bytes(&dtb).unwrap();
    let tree = adapt_to_fdt_tree(&dt);
    let root = tree.root().unwrap();
    let soc = find_child(&root, "soc");

    // Родитель (root) использует #address-cells=2.
    let parent_cells = root.cells_size().unwrap();
    let range = soc.range_to_parent(parent_cells).expect("soc имеет ranges");
    assert_eq!(range.child, 0x0);
    assert_eq!(range.parent, 0x1000_0000);
    assert_eq!(range.size, 0x0010_0000);
}

#[test]
fn real_fdt_is_compatible_matches_string_list() {
    let dtb = build_soc_dtb();
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
    let dtb = build_soc_dtb();
    let dt = DeviceTree::from_bytes(&dtb).unwrap();
    let tree = adapt_to_fdt_tree(&dt);
    let root = tree.root().unwrap();
    let soc = find_child(&root, "soc");
    let serial = find_child(&soc, "serial@2000");

    // hierarchy: root -> soc -> serial.
    let ctx = ProbeContext::new(serial, vec![root, soc, serial]);
    let reg = ctx.get_address(0);
    // 0x1000_0000 (parent) + 0x2000 (offset) - 0x0 (child) = 0x1000_2000.
    assert_eq!(reg.offset, 0x1000_2000);
    assert_eq!(reg.size, 0x1000);

    // mmio-обёртка.
    let mmio = ctx.get_mmio_address(0).expect("валидный mmio");
    assert_eq!(mmio.base(), 0x1000_2000);
}

#[test]
fn real_fdt_get_address_reg_index_out_of_bounds() {
    let dtb = build_soc_dtb();
    let dt = DeviceTree::from_bytes(&dtb).unwrap();
    let tree = adapt_to_fdt_tree(&dt);
    let root = tree.root().unwrap();
    let soc = find_child(&root, "soc");
    let serial = find_child(&soc, "serial@2000");

    let ctx = ProbeContext::new(serial, vec![root, soc, serial]);
    // reg_index=5 за пределами -> address_space по умолчанию {0,0},
    // транслируется в parent (0x1000_0000) + 0 - child(0) = 0x1000_0000.
    let reg = ctx.get_address(5);
    assert_eq!(reg.size, 0);
    assert_eq!(reg.offset, 0x1000_0000);
}

#[test]
fn real_fdt_get_address_without_parent_bus() {
    let dtb = build_soc_dtb();
    let dt = DeviceTree::from_bytes(&dtb).unwrap();
    let tree = adapt_to_fdt_tree(&dt);
    let root = tree.root().unwrap();
    let soc = find_child(&root, "soc");

    // soc как одиночный узел (нет родителя в иерархии): bus_range = default {0,0,0},
    // reg у soc нет -> address_space {0,0} -> offset 0.
    let ctx = ProbeContext::new(soc, vec![soc]);
    let reg = ctx.get_address(0);
    assert_eq!(reg.offset, 0);
    assert_eq!(reg.size, 0);
}
