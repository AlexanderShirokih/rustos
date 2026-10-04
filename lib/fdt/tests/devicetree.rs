#![allow(clippy::unwrap_used)]
#![allow(clippy::cast_possible_truncation)]

use core::num::NonZeroUsize;

use fdt::{
    devicetree::{DeviceTree, DtError, Node},
    devicetreeext::{
        AddressSpace, BUS_PATH_MAX_NODES, NodeExt, PropExt, SkippedWindow, available_path,
        device_windows, find_ram_regions, is_window_admissible, node_path, page_regions,
        reserved_memory_ranges, translate_window,
    },
};
use fdt_test::Token;

fn build_dtb_two_banks() -> Vec<u8> {
    let mut buf = Vec::new();

    let strings = b"#address-cells\0#size-cells\0device_type\0reg\0";
    let str_addr_cells: u32 = 0;
    let str_size_cells: u32 = 15;
    let str_device_type: u32 = 27;
    let str_reg: u32 = 39;

    let mut st = Vec::new();

    push_u32(&mut st, 0x1);
    push_u32(&mut st, 0x0);

    push_prop_u32(&mut st, str_addr_cells, 2);
    push_prop_u32(&mut st, str_size_cells, 2);

    push_u32(&mut st, 0x1);
    st.extend_from_slice(b"memory@80000000\0");
    align4(&mut st);

    push_prop_bytes(&mut st, str_device_type, b"memory\0");

    let mut reg = Vec::new();
    push_u32(&mut reg, 0x00);
    push_u32(&mut reg, 0x8000_0000);
    push_u32(&mut reg, 0x00);
    push_u32(&mut reg, 0x8000_0000);
    push_u32(&mut reg, 0x01);
    push_u32(&mut reg, 0x0000_0000);
    push_u32(&mut reg, 0x00);
    push_u32(&mut reg, 0x8000_0000);
    push_prop_bytes(&mut st, str_reg, &reg);

    push_u32(&mut st, 0x2);
    push_u32(&mut st, 0x2);
    push_u32(&mut st, 0x9);

    write_header(&mut buf, &st, strings);
    buf
}

fn build_dtb_with_children() -> Vec<u8> {
    let mut buf = Vec::new();

    let strings = b"#address-cells\0#size-cells\0device_type\0reg\0ranges\0";
    let str_addr_cells: u32 = 0;
    let str_size_cells: u32 = 15;
    let str_device_type: u32 = 27;
    let str_reg: u32 = 39;
    let str_ranges: u32 = 43;

    let mut st = Vec::new();

    push_u32(&mut st, 0x1);
    push_u32(&mut st, 0x0);

    push_prop_u32(&mut st, str_addr_cells, 2);
    push_prop_u32(&mut st, str_size_cells, 1);

    push_u32(&mut st, 0x1);
    st.extend_from_slice(b"memory@0\0");
    align4(&mut st);

    push_prop_bytes(&mut st, str_device_type, b"memory\0");

    let mut reg = Vec::new();
    push_u32(&mut reg, 0x00);
    push_u32(&mut reg, 0x0000_0000);
    push_u32(&mut reg, 0x4000_0000);
    push_prop_bytes(&mut st, str_reg, &reg);

    push_u32(&mut st, 0x2);

    push_u32(&mut st, 0x1);
    st.extend_from_slice(b"reserved-memory\0");
    align4(&mut st);

    push_prop_u32(&mut st, str_addr_cells, 2);
    push_prop_u32(&mut st, str_size_cells, 1);
    push_u32(&mut st, 0x3);
    push_u32(&mut st, 0);
    push_u32(&mut st, str_ranges);

    push_u32(&mut st, 0x1);
    st.extend_from_slice(b"rsv@10000000\0");
    align4(&mut st);

    let mut rsv_reg = Vec::new();
    push_u32(&mut rsv_reg, 0x00);
    push_u32(&mut rsv_reg, 0x1000_0000);
    push_u32(&mut rsv_reg, 0x1000);
    push_prop_bytes(&mut st, str_reg, &rsv_reg);

    push_u32(&mut st, 0x2);
    push_u32(&mut st, 0x2);

    push_u32(&mut st, 0x1);
    st.extend_from_slice(b"chosen\0");
    align4(&mut st);
    push_u32(&mut st, 0x2);

    push_u32(&mut st, 0x2);
    push_u32(&mut st, 0x9);

    write_header(&mut buf, &st, strings);
    buf
}

fn write_header(buf: &mut Vec<u8>, structure: &[u8], strings: &[u8]) {
    write_header_rsv(buf, structure, strings, &[]);
}

fn write_header_rsv(
    buf: &mut Vec<u8>,
    structure: &[u8],
    strings: &[u8],
    reservations: &[(u64, u64)],
) {
    let hdr: u32 = 40;
    let rsvmap = ((reservations.len() + 1) * 16) as u32;
    let off_rsvmap = hdr;
    let off_struct = off_rsvmap + rsvmap;
    let off_strings = off_struct + structure.len() as u32;
    let total = off_strings + strings.len() as u32;

    push_u32(buf, 0xD00D_FEED);
    push_u32(buf, total);
    push_u32(buf, off_struct);
    push_u32(buf, off_strings);
    push_u32(buf, off_rsvmap);
    push_u32(buf, 17);
    push_u32(buf, 16);
    push_u32(buf, 0);
    push_u32(buf, strings.len() as u32);
    push_u32(buf, structure.len() as u32);

    for &(address, size) in reservations {
        push_u64(buf, address);
        push_u64(buf, size);
    }

    buf.extend_from_slice(&[0u8; 16]);
    buf.extend_from_slice(structure);
    buf.extend_from_slice(strings);
}

fn push_u32(buf: &mut Vec<u8>, val: u32) {
    buf.extend_from_slice(&val.to_be_bytes());
}

fn push_u64(buf: &mut Vec<u8>, val: u64) {
    buf.extend_from_slice(&val.to_be_bytes());
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

fn align4(buf: &mut Vec<u8>) {
    while !buf.len().is_multiple_of(4) {
        buf.push(0);
    }
}

#[test]
fn root_and_children() {
    let dtb = build_dtb_with_children();
    let dt = DeviceTree::from_bytes(&dtb).unwrap();
    let root = dt.root().unwrap();

    let names: Vec<&str> = root.children().map(|n| n.name()).collect();

    assert_eq!(names, ["memory@0", "reserved-memory", "chosen"]);
}

#[test]
fn cells_size() {
    let dtb = build_dtb_with_children();
    let dt = DeviceTree::from_bytes(&dtb).unwrap();
    let root = dt.root().unwrap();

    let stride = root.cells_size().unwrap().stride();

    assert_eq!(stride, 3);
}

#[test]
fn memory_node_by_device_type() {
    let dtb = build_dtb_with_children();
    let dt = DeviceTree::from_bytes(&dtb).unwrap();
    let root = dt.root().unwrap();

    let mem: Vec<_> = root
        .children()
        .filter(|n| {
            n.prop("device_type")
                .and_then(|p| p.as_cstr().map(|s| s == "memory"))
                .unwrap_or(false)
        })
        .map(|n| n.name())
        .collect();

    assert_eq!(mem, ["memory@0"]);
}

#[test]
fn reg_single_entry() {
    let dtb = build_dtb_with_children();
    let dt = DeviceTree::from_bytes(&dtb).unwrap();
    let root = dt.root().unwrap();
    let cells = root.cells_size().unwrap();
    let mem = root.children().find(|n| n.name() == "memory@0").unwrap();
    let reg = mem.prop("reg").unwrap();

    let list: Vec<_> = reg
        .try_as_reg_list::<8>(cells)
        .unwrap()
        .into_iter()
        .map(|entry| (entry.offset, entry.size))
        .collect();

    assert_eq!(list, [(0x00, 0x4000_0000)]);
}

#[test]
fn first_reg_base_of_direct_child() {
    let dtb = build_dtb_with_children();
    let dt = DeviceTree::from_bytes(&dtb).unwrap();
    let root = dt.root().unwrap();
    let mem = root.children().find(|n| n.name() == "memory@0").unwrap();
    let rsv = dt.find("/reserved-memory").unwrap();
    let child = rsv.children().next().unwrap();
    let chosen = dt.find("/chosen").unwrap();

    let bases = (
        mem.first_reg_base(&root),
        child.first_reg_base(&rsv),
        chosen.first_reg_base(&root),
    );

    assert_eq!(bases, (Some(0x00), Some(0x1000_0000), None));
}

#[test]
fn find_path() {
    let dtb = build_dtb_with_children();
    let dt = DeviceTree::from_bytes(&dtb).unwrap();

    let names = [
        "/",
        "/memory@0",
        "/reserved-memory",
        "/chosen",
        "/nonexistent",
    ]
    .map(|path| dt.find(path).map(|node| node.name()));

    assert_eq!(
        names,
        [
            Some(""),
            Some("memory@0"),
            Some("reserved-memory"),
            Some("chosen"),
            None,
        ]
    );
}

#[test]
fn nested_children() {
    let dtb = build_dtb_with_children();
    let dt = DeviceTree::from_bytes(&dtb).unwrap();
    let rsv = dt.find("/reserved-memory").unwrap();
    let cells = rsv.cells_size().unwrap();

    let children: Vec<_> = rsv
        .children()
        .map(|child| {
            let reg = child
                .prop("reg")
                .unwrap()
                .try_as_reg_list::<4>(cells)
                .unwrap();
            let entries: Vec<_> = reg
                .into_iter()
                .map(|entry| (entry.offset, entry.size))
                .collect();
            (child.name(), entries)
        })
        .collect();

    assert_eq!(children, [("rsv@10000000", vec![(0x1000_0000, 0x1000)])]);
}

#[test]
fn properties_iteration() {
    let dtb = build_dtb_with_children();
    let dt = DeviceTree::from_bytes(&dtb).unwrap();
    let root = dt.root().unwrap();
    let mem = root.children().find(|n| n.name() == "memory@0").unwrap();

    let props: Vec<&str> = mem.properties().map(|p| p.name()).collect();

    assert_eq!(props, ["device_type", "reg"]);
}

#[test]
fn reg_two_banks_stride4() {
    let dtb = build_dtb_two_banks();
    let dt = DeviceTree::from_bytes(&dtb).unwrap();
    let root = dt.root().unwrap();
    let mem = root.children().next().unwrap();
    let reg = mem.prop("reg").unwrap();

    let cells = root.cells_size().unwrap();
    let stride = cells.stride();
    let value_len = reg.value().len();
    let list: Vec<_> = reg
        .try_as_reg_list::<8>(cells)
        .unwrap()
        .into_iter()
        .map(|entry| (entry.offset, entry.size))
        .collect();

    assert_eq!(stride, 4);
    assert_eq!(value_len, 32);
    assert_eq!(
        list,
        [(0x8000_0000, 0x8000_0000), (0x1_0000_0000, 0x8000_0000)]
    );
}

#[test]
fn reg_list_n_limits_result() {
    let dtb = build_dtb_two_banks();
    let dt = DeviceTree::from_bytes(&dtb).unwrap();
    let root = dt.root().unwrap();
    let cells = root.cells_size().unwrap();
    let mem = root.children().next().unwrap();
    let reg = mem.prop("reg").unwrap();

    let full: Vec<_> = reg
        .try_as_reg_list::<8>(cells)
        .unwrap()
        .into_iter()
        .map(|entry| (entry.offset, entry.size))
        .collect();
    let limited: Vec<_> = reg
        .try_as_reg_list::<1>(cells)
        .unwrap()
        .into_iter()
        .map(|entry| (entry.offset, entry.size))
        .collect();

    assert_eq!(full.len(), 2);
    assert_eq!(limited, &full[..1]);
}

fn build_dtb_with_reservations(reservations: &[(u64, u64)]) -> Vec<u8> {
    let mut buf = Vec::new();
    let strings = b"\0";

    let mut st = Vec::new();
    push_u32(&mut st, 0x1);
    push_u32(&mut st, 0x0);
    push_u32(&mut st, 0x2);
    push_u32(&mut st, 0x9);

    write_header_rsv(&mut buf, &st, strings, reservations);
    buf
}

fn build_dtb_reserved_mixed() -> Vec<u8> {
    let mut buf = Vec::new();

    let strings = b"#address-cells\0#size-cells\0reg\0size\0";
    let str_addr_cells: u32 = 0;
    let str_size_cells: u32 = 15;
    let str_reg: u32 = 27;
    let str_size: u32 = 31;

    let mut st = Vec::new();

    push_u32(&mut st, 0x1);
    push_u32(&mut st, 0x0);
    push_prop_u32(&mut st, str_addr_cells, 2);
    push_prop_u32(&mut st, str_size_cells, 1);

    push_u32(&mut st, 0x1);
    st.extend_from_slice(b"reserved-memory\0");
    align4(&mut st);
    push_prop_u32(&mut st, str_addr_cells, 2);
    push_prop_u32(&mut st, str_size_cells, 1);

    push_u32(&mut st, 0x1);
    st.extend_from_slice(b"static@20000000\0");
    align4(&mut st);
    let mut reg = Vec::new();
    push_u32(&mut reg, 0x00);
    push_u32(&mut reg, 0x2000_0000);
    push_u32(&mut reg, 0x1000);
    push_prop_bytes(&mut st, str_reg, &reg);
    push_u32(&mut st, 0x2);

    push_u32(&mut st, 0x1);
    st.extend_from_slice(b"dynamic\0");
    align4(&mut st);
    push_prop_u32(&mut st, str_size, 0x4000);
    push_u32(&mut st, 0x2);

    push_u32(&mut st, 0x2);
    push_u32(&mut st, 0x2);
    push_u32(&mut st, 0x9);

    write_header(&mut buf, &st, strings);
    buf
}

#[test]
fn mem_reservations_yields_entries_until_terminator() {
    let dtb = build_dtb_with_reservations(&[(0x1000_0000, 0x2000), (0xA000_0000, 0x4000)]);
    let dt = DeviceTree::from_bytes(&dtb).unwrap();

    let list: Vec<_> = dt
        .memory_reservations()
        .map(|reservation| (reservation.address, reservation.size))
        .collect();

    assert_eq!(list, [(0x1000_0000, 0x2000), (0xA000_0000, 0x4000)]);
}

#[test]
fn mem_reservations_empty_when_block_has_only_terminator() {
    let dtb = build_dtb_with_reservations(&[]);
    let dt = DeviceTree::from_bytes(&dtb).unwrap();

    let count = dt.memory_reservations().count();

    assert_eq!(count, 0);
}

#[test]
fn mem_reservations_stops_at_buffer_end() {
    let mut dtb = fdt_test::build(&[]);
    let reservations_at = dtb.len() as u32;
    push_u64(&mut dtb, 0x1000_0000);
    push_u64(&mut dtb, 0x2000);
    let total = (dtb.len() as u32).to_be_bytes();
    dtb[4..8].copy_from_slice(&total);
    dtb[16..20].copy_from_slice(&reservations_at.to_be_bytes());
    let dt = DeviceTree::from_bytes(&dtb).unwrap();

    let list: Vec<_> = dt
        .memory_reservations()
        .map(|reservation| (reservation.address, reservation.size))
        .collect();

    assert_eq!(list, [(0x1000_0000, 0x2000)]);
}

#[test]
fn reserved_memory_dynamic_child_has_size_but_no_reg() {
    let dtb = build_dtb_reserved_mixed();
    let dt = DeviceTree::from_bytes(&dtb).unwrap();
    let reserved = dt.find("/reserved-memory").unwrap();
    let dynamic = reserved
        .children()
        .find(|node| node.name() == "dynamic")
        .unwrap();

    let present = ["reg", "size"].map(|name| dynamic.prop(name).is_some());

    assert_eq!(present, [false, true]);
}

#[test]
fn reserved_memory_ranges_yields_static_only() {
    let dtb = build_dtb_reserved_mixed();
    let dt = DeviceTree::from_bytes(&dtb).unwrap();

    let ranges: Vec<_> = reserved_memory_ranges(&dt)
        .unwrap()
        .map(|reserved| (reserved.range.offset, reserved.range.size, reserved.no_map))
        .collect();

    assert_eq!(ranges, [(0x2000_0000, 0x1000, false)]);
}

#[test]
fn find_ram_regions_yields_both_banks() {
    let dtb = build_dtb_two_banks();
    let dt = DeviceTree::from_bytes(&dtb).unwrap();

    let banks: Vec<_> = find_ram_regions(&dt)
        .unwrap()
        .map(|bank| (bank.offset, bank.size))
        .collect();

    assert_eq!(
        banks,
        [(0x8000_0000, 0x8000_0000), (0x1_0000_0000, 0x8000_0000)]
    );
}

#[test]
fn reserved_memory_ranges_none_without_node() {
    let dtb = build_dtb_two_banks();
    let dt = DeviceTree::from_bytes(&dtb).unwrap();

    let found = reserved_memory_ranges(&dt).is_some();

    assert!(!found);
}

#[test]
fn from_bytes_rejects_invalid_magic() {
    let mut dtb = build_dtb_with_children();
    dtb[0] ^= 0xFF;

    let parsed = DeviceTree::from_bytes(&dtb);

    assert!(matches!(parsed, Err(DtError::InvalidMagic(_))));
}

#[test]
fn from_bytes_rejects_truncated_header() {
    let dtb = build_dtb_with_children();
    let short = &dtb[..10];

    let parsed = DeviceTree::from_bytes(short);

    assert!(matches!(
        parsed,
        Err(DtError::Incomplete {
            total_size: 40,
            actual: 10,
        })
    ));
}

#[test]
fn from_bytes_rejects_inflated_total_size() {
    let mut dtb = build_dtb_with_children();
    let actual_size = dtb.len();
    let inflated = (actual_size as u32 + 4096).to_be_bytes();
    dtb[4..8].copy_from_slice(&inflated);

    let parsed = DeviceTree::from_bytes(&dtb);

    assert!(matches!(
        parsed,
        Err(DtError::Incomplete { total_size, actual })
            if total_size == actual_size + 4096 && actual == actual_size
    ));
}

#[test]
fn a_header_before_version_17_bounds_the_structure_by_the_blob() {
    let mut dtb = fdt_test::build(PATH_TREE);
    dtb[20..24].copy_from_slice(&16u32.to_be_bytes());
    dtb[36..40].copy_from_slice(&0u32.to_be_bytes());

    let walked = DeviceTree::from_bytes(&dtb).map(|tree| tree.walk().count());

    assert!(matches!(walked, Ok(6)));
}

#[test]
fn a_version_outside_2_to_17_is_unsupported() {
    let mut older = fdt_test::build(&[]);
    older[20..24].copy_from_slice(&1u32.to_be_bytes());
    let mut newer = fdt_test::build(&[]);
    newer[24..28].copy_from_slice(&18u32.to_be_bytes());

    let older_parsed = DeviceTree::from_bytes(&older).map(drop);
    let newer_parsed = DeviceTree::from_bytes(&newer).map(drop);

    assert!(matches!(
        older_parsed,
        Err(DtError::UnsupportedVersion { version: 1, .. })
    ));
    assert!(matches!(
        newer_parsed,
        Err(DtError::UnsupportedVersion {
            last_compatible: 18,
            ..
        })
    ));
}

#[test]
fn a_property_length_past_the_structure_block_is_rejected() {
    let mut st = Vec::new();
    push_u32(&mut st, 0x1);
    push_u32(&mut st, 0x0);
    push_u32(&mut st, 0x3);
    push_u32(&mut st, 16);
    push_u32(&mut st, 0);
    push_u32(&mut st, 0xDEAD_BEEF);
    push_u32(&mut st, 0x2);
    push_u32(&mut st, 0x9);
    let mut dtb = Vec::new();
    write_header(&mut dtb, &st, b"reg\0");

    let parsed = DeviceTree::from_bytes(&dtb);

    assert!(matches!(parsed, Err(DtError::Malformed { .. })));
}

#[test]
fn a_property_name_without_terminator_in_the_strings_block_is_rejected() {
    let mut st = Vec::new();
    push_u32(&mut st, 0x1);
    push_u32(&mut st, 0x0);
    push_prop_u32(&mut st, 0, 7);
    push_u32(&mut st, 0x2);
    push_u32(&mut st, 0x9);
    let mut dtb = Vec::new();
    write_header(&mut dtb, &st, b"reg");

    let parsed = DeviceTree::from_bytes(&dtb);

    assert!(matches!(parsed, Err(DtError::Malformed { .. })));
}

#[test]
fn a_structure_block_cut_before_its_end_token_is_rejected() {
    let mut st = Vec::new();
    push_u32(&mut st, 0x1);
    push_u32(&mut st, 0x0);
    push_prop_u32(&mut st, 0, 7);
    let mut dtb = Vec::new();
    write_header(&mut dtb, &st, b"reg\0");

    let parsed = DeviceTree::from_bytes(&dtb);

    assert!(matches!(parsed, Err(DtError::Malformed { .. })));
}

#[test]
fn an_unknown_token_between_nodes_is_rejected() {
    let mut st = Vec::new();
    push_u32(&mut st, 0x1);
    push_u32(&mut st, 0x0);
    push_u32(&mut st, 0x1);
    st.extend_from_slice(b"a\0\0\0");
    push_u32(&mut st, 0x2);
    push_u32(&mut st, 0x7);
    push_u32(&mut st, 0x1);
    st.extend_from_slice(b"b\0\0\0");
    push_u32(&mut st, 0x2);
    push_u32(&mut st, 0x2);
    push_u32(&mut st, 0x9);
    let mut dtb = Vec::new();
    write_header(&mut dtb, &st, b"");

    let parsed = DeviceTree::from_bytes(&dtb);

    assert!(matches!(parsed, Err(DtError::Malformed { .. })));
}

fn build_dtb_with_aliases_and_bus() -> Vec<u8> {
    let mut strings: Vec<u8> = Vec::new();
    let mut intern = |name: &str| -> u32 {
        let off = strings.len() as u32;
        strings.extend_from_slice(name.as_bytes());
        strings.push(0);
        off
    };
    let s_serial0 = intern("serial0");
    let s_addr = intern("#address-cells");
    let s_size = intern("#size-cells");
    let s_ranges = intern("ranges");
    let s_reg = intern("reg");

    let mut st = Vec::new();
    push_u32(&mut st, 0x1);
    push_u32(&mut st, 0x0);

    push_u32(&mut st, 0x1);
    st.extend_from_slice(b"aliases\0");
    align4(&mut st);
    push_prop_bytes(&mut st, s_serial0, b"/soc/serial@1000\0");
    push_u32(&mut st, 0x2);

    push_u32(&mut st, 0x1);
    st.extend_from_slice(b"soc\0");
    align4(&mut st);
    push_prop_u32(&mut st, s_addr, 1);
    push_prop_u32(&mut st, s_size, 1);
    let mut ranges = Vec::new();
    push_u32(&mut ranges, 0x0);
    push_u32(&mut ranges, 0x0);
    push_u32(&mut ranges, 0x1000_0000);
    push_u32(&mut ranges, 0x0010_0000);
    push_prop_bytes(&mut st, s_ranges, &ranges);

    push_u32(&mut st, 0x1);
    st.extend_from_slice(b"serial@1000\0");
    align4(&mut st);
    let mut reg = Vec::new();
    push_u32(&mut reg, 0x1000);
    push_u32(&mut reg, 0x100);
    push_prop_bytes(&mut st, s_reg, &reg);
    push_u32(&mut st, 0x2);

    push_u32(&mut st, 0x2);
    push_u32(&mut st, 0x2);
    push_u32(&mut st, 0x9);

    let mut buf = Vec::new();
    write_header(&mut buf, &st, &strings);
    buf
}

#[test]
fn find_by_alias_resolves_to_node() {
    let dtb = build_dtb_with_aliases_and_bus();
    let dt = DeviceTree::from_bytes(&dtb).unwrap();
    let serial = dt.find("/soc/serial@1000").unwrap().key();

    let found = ["serial0", "nonexistent_alias"]
        .map(|alias| dt.find(alias).map(|node| (node.name(), node.key())));

    assert_eq!(found, [Some(("serial@1000", serial)), None]);
}

fn build_dtb_with_scalar_props() -> Vec<u8> {
    let mut strings: Vec<u8> = Vec::new();
    let mut intern = |name: &str| -> u32 {
        let off = strings.len() as u32;
        strings.extend_from_slice(name.as_bytes());
        strings.push(0);
        off
    };
    let s4 = intern("val4");
    let s8 = intern("val8");
    let s3 = intern("val3");

    let mut st = Vec::new();
    push_u32(&mut st, 0x1);
    push_u32(&mut st, 0x0);
    push_prop_bytes(&mut st, s4, &0x1234_5678u32.to_be_bytes());
    push_prop_bytes(&mut st, s8, &0x1122_3344_5566_7788u64.to_be_bytes());
    push_prop_bytes(&mut st, s3, &[0xAA, 0xBB, 0xCC]);
    push_u32(&mut st, 0x2);
    push_u32(&mut st, 0x9);

    let mut buf = Vec::new();
    write_header(&mut buf, &st, &strings);
    buf
}

#[test]
fn property_scalar_conversions() {
    let dtb = build_dtb_with_scalar_props();
    let dt = DeviceTree::from_bytes(&dtb).unwrap();
    let root = dt.root().unwrap();
    let v4 = root.prop("val4").unwrap();
    let v8 = root.prop("val8").unwrap();
    let v3 = root.prop("val3").unwrap();

    let four = (v4.as_usize(), v4.try_as_u32(0), v4.try_as_u64(0));
    let eight = (v8.try_as_u64(0), v8.as_usize());
    let three = (v3.as_usize(), v3.try_as_u32(0), v3.try_as_u64(0));

    assert_eq!(four, (0x1234_5678, Some(0x1234_5678), None));
    assert_eq!(eight, (Some(0x1122_3344_5566_7788), 0x1122_3344_5566_7788));
    assert_eq!(three, (0, None, None));
}

fn build_dtb_deeply_nested() -> Vec<u8> {
    let strings = b"\0";
    let mut st = Vec::new();
    push_u32(&mut st, 0x1);
    push_u32(&mut st, 0x0);
    for name in [b"a\0".as_slice(), b"b\0", b"c\0", b"leaf\0"] {
        push_u32(&mut st, 0x1);
        st.extend_from_slice(name);
        align4(&mut st);
    }
    for _ in 0..5 {
        push_u32(&mut st, 0x2);
    }
    push_u32(&mut st, 0x9);

    let mut buf = Vec::new();
    write_header(&mut buf, &st, strings);
    buf
}

#[test]
fn find_traverses_more_than_two_levels() {
    let dtb = build_dtb_deeply_nested();
    let dt = DeviceTree::from_bytes(&dtb).unwrap();
    let a = dt.find("/a").unwrap();

    let found =
        ["/a/b/c/leaf", "/a/b/c", "/a/b/x"].map(|path| dt.find(path).map(|node| node.name()));
    let descended = a
        .children()
        .next()
        .and_then(|b| b.children().next())
        .and_then(|c| c.children().next())
        .map(|leaf| (leaf.name(), leaf.children().count()));

    assert_eq!(found, [Some("leaf"), Some("c"), None]);
    assert_eq!(descended, Some(("leaf", 0)));
}

#[test]
fn the_walk_lists_every_node_with_its_depth_in_description_order() {
    let dtb = fdt_test::build(PATH_TREE);
    let dt = DeviceTree::from_bytes(&dtb).unwrap();

    let walked: Vec<_> = dt
        .walk()
        .map(|(depth, node)| (depth, node.name()))
        .collect();

    assert_eq!(
        walked,
        [
            (0, ""),
            (1, "aliases"),
            (1, "soc"),
            (2, "serial@1000"),
            (2, "off"),
            (3, "leaf"),
        ]
    );
}

#[test]
fn only_a_node_without_status_or_with_okay_is_available() {
    let dtb = fdt_test::build(&[
        Token::Node("absent"),
        Token::End,
        Token::Node("okay"),
        Token::Strings("status", &["okay"]),
        Token::End,
        Token::Node("ok"),
        Token::Strings("status", &["ok"]),
        Token::End,
        Token::Node("padded"),
        Token::Bytes("status", b"okay\0\0"),
        Token::End,
        Token::Node("disabled"),
        Token::Strings("status", &["disabled"]),
        Token::End,
        Token::Node("fail"),
        Token::Strings("status", &["fail"]),
        Token::End,
    ]);
    let dt = DeviceTree::from_bytes(&dtb).unwrap();

    let available: Vec<_> = dt
        .root()
        .unwrap()
        .children()
        .filter(NodeExt::is_available)
        .map(|node| node.name())
        .collect();

    assert_eq!(available, ["absent", "okay", "ok", "padded"]);
}

#[test]
fn a_string_value_ends_at_its_first_nul() {
    let dtb = fdt_test::build(&[
        Token::Bytes("terminated", b"serial\0"),
        Token::Bytes("two", b"serial\0uart\0"),
        Token::Bytes("unterminated", b"serial"),
        Token::Bytes("empty", b""),
        Token::Bytes("binary", b"\xff\0"),
    ]);
    let dt = DeviceTree::from_bytes(&dtb).unwrap();
    let root = dt.root().unwrap();

    let texts = ["terminated", "two", "unterminated", "empty", "binary"]
        .map(|name| root.prop(name).unwrap().as_cstr());

    assert_eq!(texts, [Some("serial"), Some("serial"), None, None, None]);
}

#[test]
fn a_string_list_skips_empty_strings_and_is_checked_as_a_whole() {
    let dtb = fdt_test::build(&[
        Token::Strings("list", &["arm,pl031", "arm,primecell"]),
        Token::Bytes("padded", b"arm,pl031\0\0"),
        Token::Bytes("unterminated", b"arm,pl031\0arm"),
        Token::Bytes("binary", b"arm,pl031\0\xff\0"),
    ]);
    let dt = DeviceTree::from_bytes(&dtb).unwrap();
    let root = dt.root().unwrap();

    let lists = ["list", "padded", "unterminated", "binary"].map(|name| {
        root.prop(name)
            .unwrap()
            .into_string_list_iter()
            .map(Iterator::collect::<Vec<_>>)
    });

    assert_eq!(
        lists,
        [
            Some(vec!["arm,pl031", "arm,primecell"]),
            Some(vec!["arm,pl031"]),
            None,
            None,
        ]
    );
}

const PATH_TREE: &[Token<'static>] = &fdt_test::tree! {
    aliases {
        serial0 = "/soc/serial@1000";
    };
    soc {
        serial@1000 {};
        off {
            status = "disabled";
            leaf {};
        };
    };
};

#[test]
fn the_available_path_holds_the_node_and_its_ancestors_from_the_root() {
    let dtb = fdt_test::build(PATH_TREE);
    let dt = DeviceTree::from_bytes(&dtb).unwrap();

    let names = ["/soc/serial@1000", "/"].map(|path| {
        available_path(&dt, path).map(|found| {
            (
                found.node().name(),
                found.ancestors().iter().map(Node::name).collect::<Vec<_>>(),
            )
        })
    });

    assert_eq!(
        names,
        [Some(("serial@1000", vec!["", "soc"])), Some(("", vec![]))]
    );
}

#[test]
fn a_node_under_a_disabled_node_has_a_path_but_no_available_path() {
    let dtb = fdt_test::build(PATH_TREE);
    let dt = DeviceTree::from_bytes(&dtb).unwrap();

    let found = ["/soc/off", "/soc/off/leaf"].map(|path| {
        (
            node_path(&dt, path).is_some(),
            available_path(&dt, path).is_some(),
        )
    });

    assert_eq!(found, [(true, false), (true, false)]);
}

#[test]
fn an_alias_an_empty_component_or_a_missing_name_finds_no_node() {
    let dtb = fdt_test::build(PATH_TREE);
    let dt = DeviceTree::from_bytes(&dtb).unwrap();

    let found = ["serial0", "/soc//serial@1000", "/soc/serial@2000"]
        .map(|path| node_path(&dt, path).is_some());

    assert_eq!(found, [false; 3]);
}

#[test]
fn a_node_past_the_longest_path_has_no_path() {
    let path = |buses| format!("{}/device", "/bus".repeat(buses));
    let longest = build_dtb_with_bus_chain(BUS_PATH_MAX_NODES - 2);
    let past = build_dtb_with_bus_chain(BUS_PATH_MAX_NODES - 1);
    let longest = DeviceTree::from_bytes(&longest).unwrap();
    let past = DeviceTree::from_bytes(&past).unwrap();

    let longest_found = node_path(&longest, &path(BUS_PATH_MAX_NODES - 2)).is_some();
    let past_found = node_path(&past, &path(BUS_PATH_MAX_NODES - 1)).is_some();

    assert!(longest_found);
    assert!(!past_found);
}

fn build_dtb_with_buses() -> Vec<u8> {
    fdt_test::build(&fdt_test::tree! {
        #address-cells = <2>;
        #size-cells = <2>;
        identity {
            #address-cells = <1>;
            #size-cells = <1>;
            ranges;
            dev {
                reg = <0x1000 0x100>;
            };
        };
        shifted {
            #address-cells = <1>;
            #size-cells = <1>;
            ranges = <0x0 0x0 0x1000_0000 0x10_0000>;
            dev {
                reg = <0x2000 0x1000>;
            };
            tail {
                reg = <0xf_f800 0x1000>;
            };
        };
        split {
            #address-cells = <1>;
            #size-cells = <1>;
            ranges = <0x0 0x0 0x1000_0000 0x1000 0x10_0000 0x0 0x2000_0000 0x1000>;
            dev {
                reg = <0x10_0800 0x100>;
            };
        };
        closed {
            #address-cells = <1>;
            #size-cells = <1>;
            dev {
                reg = <0x40 0x10>;
            };
        };
        outer {
            #address-cells = <1>;
            #size-cells = <1>;
            ranges = <0x0 0x0 0x4000_0000 0x100_0000>;
            inner {
                #address-cells = <1>;
                #size-cells = <1>;
                ranges = <0x0 0x10_0000 0x1_0000>;
                dev {
                    reg = <0x20 0x10>;
                };
            };
        };
        top {
            #address-cells = <1>;
            #size-cells = <1>;
            ranges = <0x0 0xffff_ffff 0xffff_f000 0x10_0000>;
            dev {
                reg = <0x2000 0x1000>;
            };
        };
        pci {
            #address-cells = <3>;
            #size-cells = <2>;
            ranges = <0x0200_0000 0x0 0x0 0x0 0x3000_0000 0x0 0x10_0000>;
            dev {
                reg = <0x0 0x0 0x1000 0x0 0x100>;
            };
        };
    })
}

fn bus_path<'a>(dt: &'a DeviceTree<'a>, device: &str) -> Vec<Node<'a>> {
    let (buses, _) = device.rsplit_once('/').unwrap();
    let mut path = vec![dt.root().unwrap()];
    for name in buses.split('/').filter(|name| !name.is_empty()) {
        let bus = path
            .last()
            .unwrap()
            .children()
            .find(|child| child.name() == name);
        path.push(bus.unwrap());
    }
    path
}

fn first_window(bus: &Node<'_>, device: &Node<'_>) -> AddressSpace {
    let cells = bus.cells_size().unwrap();
    let reg = device
        .prop("reg")
        .unwrap()
        .try_as_reg_list::<4>(cells)
        .unwrap();
    *reg.get(0).unwrap()
}

fn translated_first_window(dt: &DeviceTree<'_>, device: &str) -> Option<(usize, usize)> {
    let path = bus_path(dt, device);
    let window = first_window(path.last()?, &dt.find(device)?);
    translate_window(&path, window).map(|window| (window.offset, window.size))
}

#[test]
fn an_empty_ranges_keeps_the_window() {
    let dtb = build_dtb_with_buses();
    let dt = DeviceTree::from_bytes(&dtb).unwrap();

    let window = translated_first_window(&dt, "/identity/dev");

    assert_eq!(window, Some((0x1000, 0x100)));
}

#[test]
fn a_ranges_entry_shifts_the_window_into_the_parent_space() {
    let dtb = build_dtb_with_buses();
    let dt = DeviceTree::from_bytes(&dtb).unwrap();

    let window = translated_first_window(&dt, "/shifted/dev");

    assert_eq!(window, Some((0x1000_2000, 0x1000)));
}

#[test]
fn the_window_is_translated_by_the_entry_that_contains_it() {
    let dtb = build_dtb_with_buses();
    let dt = DeviceTree::from_bytes(&dtb).unwrap();

    let window = translated_first_window(&dt, "/split/dev");

    assert_eq!(window, Some((0x2000_0800, 0x100)));
}

#[test]
fn a_window_past_the_end_of_the_entry_is_untranslatable() {
    let dtb = build_dtb_with_buses();
    let dt = DeviceTree::from_bytes(&dtb).unwrap();

    let window = translated_first_window(&dt, "/shifted/tail");

    assert_eq!(window, None);
}

#[test]
fn a_window_behind_a_bus_without_ranges_is_untranslatable() {
    let dtb = build_dtb_with_buses();
    let dt = DeviceTree::from_bytes(&dtb).unwrap();

    let window = translated_first_window(&dt, "/closed/dev");

    assert_eq!(window, None);
}

#[test]
fn nested_buses_compose_their_ranges() {
    let dtb = build_dtb_with_buses();
    let dt = DeviceTree::from_bytes(&dtb).unwrap();

    let window = translated_first_window(&dt, "/outer/inner/dev");

    assert_eq!(window, Some((0x4010_0020, 0x10)));
}

#[test]
fn a_translated_window_past_the_address_width_is_untranslatable() {
    let dtb = build_dtb_with_buses();
    let dt = DeviceTree::from_bytes(&dtb).unwrap();

    let window = translated_first_window(&dt, "/top/dev");

    assert_eq!(window, None);
}

#[test]
fn a_bus_with_three_address_cells_is_untranslatable() {
    let dtb = build_dtb_with_buses();
    let dt = DeviceTree::from_bytes(&dtb).unwrap();

    let window = translated_first_window(&dt, "/pci/dev");

    assert_eq!(window, None);
}

fn build_dtb_with_carveouts() -> Vec<u8> {
    fdt_test::build(&fdt_test::tree! {
        #address-cells = <1>;
        #size-cells = <1>;
        memory@80000000 {
            reg = <0x8000_0000 0x1000_0000>;
        };
        reserved-memory {
            #address-cells = <1>;
            #size-cells = <1>;
            ranges;
            carveout@84000000 {
                reg = <0x8400_0000 0x10_0000>;
                no-map;
            };
            ramoops@88000000 {
                reg = <0x8800_0000 0x10_0000>;
            };
        };
    })
}

fn is_admissible(offset: usize, size: usize) -> bool {
    let dtb = build_dtb_with_carveouts();
    let dt = DeviceTree::from_bytes(&dtb).unwrap();

    is_window_admissible(&dt, AddressSpace { offset, size })
}

#[test]
fn reserved_memory_ranges_report_no_map() {
    let dtb = build_dtb_with_carveouts();
    let dt = DeviceTree::from_bytes(&dtb).unwrap();

    let ranges: Vec<_> = reserved_memory_ranges(&dt)
        .unwrap()
        .map(|reserved| (reserved.range.offset, reserved.no_map))
        .collect();

    assert_eq!(ranges, [(0x8400_0000, true), (0x8800_0000, false)]);
}

#[test]
fn a_window_outside_ram_is_admissible() {
    let admissible = is_admissible(0x0900_0000, 0x1000);

    assert!(admissible);
}

#[test]
fn a_window_inside_a_no_map_region_is_admissible() {
    let admissible = is_admissible(0x8400_0000, 0x10_0000);

    assert!(admissible);
}

#[test]
fn a_window_over_plain_ram_is_rejected() {
    let admissible = is_admissible(0x8200_0000, 0x1000);

    assert!(!admissible);
}

#[test]
fn a_window_across_a_no_map_edge_is_rejected() {
    let admissible = is_admissible(0x840f_f000, 0x2000);

    assert!(!admissible);
}

#[test]
fn a_window_inside_a_mapped_reservation_is_rejected() {
    let admissible = is_admissible(0x8800_0000, 0x1000);

    assert!(!admissible);
}

#[test]
fn a_window_past_the_address_width_is_rejected() {
    let admissible = is_admissible(usize::MAX - 0xfff, 0x2000);

    assert!(!admissible);
}

const DEVICES_TREE: &[Token<'static>] = &fdt_test::tree! {
    #address-cells = <1>;
    #size-cells = <1>;
    uart@1000 {
        compatible;
        reg = <0x1000 0x100>;
    };
    claimed@2000 {
        compatible;
        #address-cells = <1>;
        #size-cells = <1>;
        ranges;
        reg = <0x2000 0x1000>;
        child@3000 {
            compatible;
            reg = <0x3000 0x1000>;
        };
    };
    soc {
        compatible;
        #address-cells = <1>;
        #size-cells = <1>;
        ranges = <0x0 0x1000_0000 0x10_0000>;
        timer@4000 {
            compatible;
            reg = <0x4000 0x100 0x5000 0x0>;
        };
        serial-bus@6000 {
            compatible;
            #address-cells = <1>;
            #size-cells = <1>;
            reg = <0x6000 0x100>;
            device@0 {
                compatible;
                reg = <0x0 0x10>;
            };
        };
    };
    reserved-memory {
        #address-cells = <1>;
        #size-cells = <1>;
        ranges;
        pool@8000000 {
            compatible;
            reg = <0x800_0000 0x10_0000>;
        };
    };
    chosen {
        #address-cells = <1>;
        #size-cells = <1>;
        ranges;
        framebuffer@9000 {
            compatible;
            reg = <0x9000 0x2000>;
        };
    };
};

fn windows_excluding(dtb: &[u8], excluded: &str) -> Vec<(usize, usize, bool)> {
    let dt = DeviceTree::from_bytes(dtb).unwrap();

    device_windows(dt.root().unwrap(), |node| node.name() == excluded)
        .map(|window| (window.window.offset, window.window.size, window.excluded))
        .collect()
}

#[test]
fn device_windows_are_translated_into_the_root_space() {
    let dtb = fdt_test::build(DEVICES_TREE);

    let windows = windows_excluding(&dtb, "");

    assert_eq!(
        windows,
        [
            (0x1000, 0x100, false),
            (0x2000, 0x1000, false),
            (0x3000, 0x1000, false),
            (0x1000_4000, 0x100, false),
            (0x1000_6000, 0x100, false),
            (0x9000, 0x2000, false),
        ]
    );
}

#[test]
fn an_excluded_node_and_its_subtree_give_marked_windows() {
    let dtb = fdt_test::build(DEVICES_TREE);

    let windows = windows_excluding(&dtb, "claimed@2000");

    assert_eq!(
        windows,
        [
            (0x1000, 0x100, false),
            (0x2000, 0x1000, true),
            (0x3000, 0x1000, true),
            (0x1000_4000, 0x100, false),
            (0x1000_6000, 0x100, false),
            (0x9000, 0x2000, false)
        ]
    );
}

fn build_dtb_with_bus_chain(buses: usize) -> Vec<u8> {
    let bus = [
        Token::Node("bus"),
        Token::Cells("#address-cells", &[1]),
        Token::Cells("#size-cells", &[1]),
        Token::Cells("ranges", &[]),
    ];
    let device = [
        Token::Node("device"),
        Token::Cells("compatible", &[]),
        Token::Cells("reg", &[0x1000, 0x100]),
        Token::End,
    ];

    let mut tokens = vec![
        Token::Cells("#address-cells", &[1]),
        Token::Cells("#size-cells", &[1]),
    ];
    for _ in 0..buses {
        tokens.extend_from_slice(&bus);
    }
    tokens.extend_from_slice(&device);
    tokens.extend((0..buses).map(|_| Token::End));
    fdt_test::build(&tokens)
}

#[test]
fn a_device_under_the_longest_bus_path_has_windows() {
    let dtb = build_dtb_with_bus_chain(BUS_PATH_MAX_NODES - 1);

    let windows = windows_excluding(&dtb, "");

    assert_eq!(windows, [(0x1000, 0x100, false)]);
}

#[test]
fn a_device_past_the_longest_bus_path_has_no_windows() {
    let dtb = build_dtb_with_bus_chain(BUS_PATH_MAX_NODES);

    let windows = windows_excluding(&dtb, "");

    assert!(windows.is_empty());
}

#[test]
fn reserved_memory_children_have_no_windows() {
    let dtb = fdt_test::build(DEVICES_TREE);

    let pool_listed = windows_excluding(&dtb, "")
        .iter()
        .any(|(offset, _, _)| *offset == 0x800_0000);

    assert!(!pool_listed);
}

const PAGE_SIZE: NonZeroUsize = NonZeroUsize::new(0x1000).unwrap();

const REGIONS_TREE: &[Token<'static>] = &fdt_test::tree! {
    #address-cells = <1>;
    #size-cells = <1>;
    memory@80000000 {
        device_type = "memory";
        reg = <0x8000_0000 0xfff_f800>;
    };
    rtc@9010200 {
        compatible;
        reg = <0x901_0200 0x100>;
    };
    claimed@9030000 {
        compatible;
        reg = <0x903_0000 0x100>;
    };
    sensor@9030800 {
        compatible;
        reg = <0x903_0800 0x100>;
    };
    virtio@a000000 {
        compatible;
        reg = <0xa00_0000 0x200 0xa00_0200 0x200 0xa00_0400 0x200 0xa00_0600 0x200>;
    };
    virtio@a000800 {
        compatible;
        reg = <0xa00_0800 0x200 0xa00_0a00 0x200 0xa00_0c00 0x200 0xa00_0e00 0x200>;
    };
    left@10000000 {
        compatible;
        reg = <0x1000_0000 0x1000>;
    };
    right@10001000 {
        compatible;
        reg = <0x1000_1000 0x1000>;
    };
    edge {
        compatible;
        reg = <0x8fff_fc00 0x100>;
    };
    display@b000000 {
        compatible;
        reg = <0xb00_0000 0x3000>;
    };
    overlay@b001000 {
        compatible;
        reg = <0xb00_1000 0x100>;
    };
    span@c000f80 {
        compatible;
        reg = <0xc00_0f80 0x100>;
    };
    claimed-bus@d000000 {
        compatible;
        #address-cells = <1>;
        #size-cells = <1>;
        ranges;
        reg = <0xd00_0000 0x100>;
        child@d001000 {
            compatible;
            reg = <0xd00_1000 0x100>;
        };
    };
    probe@d001800 {
        compatible;
        reg = <0xd00_1800 0x100>;
    };
};

struct PageRegions {
    regions: Vec<(usize, usize)>,
    skipped: Vec<(usize, usize, SkippedWindow)>,
}

impl PageRegions {
    fn build() -> Self {
        let dtb = fdt_test::build(REGIONS_TREE);
        let dt = DeviceTree::from_bytes(&dtb).unwrap();
        let mut windows: Vec<_> = device_windows(dt.root().unwrap(), |node| {
            node.name().starts_with("claimed")
        })
        .collect();
        let mut skipped = Vec::new();

        let regions = page_regions(&dt, &mut windows, PAGE_SIZE, |window, reason| {
            skipped.push((window.offset, window.size, reason));
        })
        .map(|region| (region.offset, region.size))
        .collect();

        Self { regions, skipped }
    }

    fn regions_within(&self, start: usize, end: usize) -> Vec<(usize, usize)> {
        self.regions
            .iter()
            .copied()
            .filter(|(offset, size)| *offset < end && start < offset + size)
            .collect()
    }
}

#[test]
fn a_window_is_widened_to_its_pages() {
    let regions = PageRegions::build().regions;

    assert!(regions.contains(&(0x901_0000, 0x1000)));
}

#[test]
fn windows_sharing_a_page_form_one_region() {
    let regions = PageRegions::build().regions_within(0xa00_0000, 0xa00_1000);

    assert_eq!(regions, [(0xa00_0000, 0x1000)]);
}

#[test]
fn touching_windows_form_separate_regions() {
    let regions = PageRegions::build().regions_within(0x1000_0000, 0x1000_2000);

    assert_eq!(regions, [(0x1000_0000, 0x1000), (0x1000_1000, 0x1000)]);
}

#[test]
fn a_window_inside_another_window_keeps_the_outer_region() {
    let regions = PageRegions::build().regions_within(0xb00_0000, 0xb00_3000);

    assert_eq!(regions, [(0xb00_0000, 0x3000)]);
}

#[test]
fn a_window_across_a_page_boundary_takes_both_pages() {
    let regions = PageRegions::build().regions_within(0xc00_0000, 0xc00_2000);

    assert_eq!(regions, [(0xc00_0000, 0x2000)]);
}

#[test]
fn a_window_in_a_page_of_an_excluded_node_is_skipped() {
    let regions = PageRegions::build();
    let page = regions.regions_within(0x903_0000, 0x903_1000);

    assert!(page.is_empty());
    assert!(
        regions
            .skipped
            .contains(&(0x903_0800, 0x100, SkippedWindow::SharesExcludedPage))
    );
}

#[test]
fn a_window_in_a_page_of_an_excluded_node_descendant_is_skipped() {
    let regions = PageRegions::build();
    let page = regions.regions_within(0xd00_0000, 0xd00_2000);

    assert!(page.is_empty());
    assert!(
        regions
            .skipped
            .contains(&(0xd00_1800, 0x100, SkippedWindow::SharesExcludedPage))
    );
}

#[test]
fn a_window_whose_page_overlaps_ram_is_skipped() {
    let regions = PageRegions::build();
    let page = regions.regions_within(0x8fff_f000, 0x9000_0000);

    assert!(page.is_empty());
    assert!(
        regions
            .skipped
            .contains(&(0x8fff_fc00, 0x100, SkippedWindow::Inadmissible))
    );
}

#[test]
fn regions_are_ordered_by_base_and_disjoint() {
    let regions = PageRegions::build().regions;
    let ordered = regions
        .windows(2)
        .all(|pair| matches!(pair, [left, right] if left.0 + left.1 <= right.0));

    assert!(ordered);
}
