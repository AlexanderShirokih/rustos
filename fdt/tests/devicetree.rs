use fdt::devicetree::DeviceTree;
use fdt::devicetreeext::{NodeExt, PropExt};

// ─── Построение DTB ─────────────────────────────────────────────────────────

/// ```text
/// / {
///     #address-cells = <2>;
///     #size-cells = <2>;
///     memory@80000000 {
///         device_type = "memory";
///         reg = <0x00 0x80000000 0x00 0x80000000
///                0x01 0x00000000 0x00 0x80000000>;
///     };
/// };
/// ```
fn build_dtb_two_banks() -> Vec<u8> {
    let mut buf = Vec::new();

    let strings = b"#address-cells\0#size-cells\0device_type\0reg\0";
    let str_addr_cells: u32 = 0;
    let str_size_cells: u32 = 15;
    let str_device_type: u32 = 27;
    let str_reg: u32 = 39;

    let mut st = Vec::new();

    // root
    push_u32(&mut st, 0x1);
    push_u32(&mut st, 0x0);

    push_prop_u32(&mut st, str_addr_cells, 2);
    push_prop_u32(&mut st, str_size_cells, 2);

    // memory@80000000
    push_u32(&mut st, 0x1);
    st.extend_from_slice(b"memory@80000000\0");
    align4(&mut st);

    push_prop_bytes(&mut st, str_device_type, b"memory\0");

    let mut reg = Vec::new();
    // Bank 1: 0x00_80000000, size 0x00_80000000
    push_u32(&mut reg, 0x00);
    push_u32(&mut reg, 0x80000000);
    push_u32(&mut reg, 0x00);
    push_u32(&mut reg, 0x80000000);
    // Bank 2: 0x01_00000000, size 0x00_80000000
    push_u32(&mut reg, 0x01);
    push_u32(&mut reg, 0x00000000);
    push_u32(&mut reg, 0x00);
    push_u32(&mut reg, 0x80000000);
    push_prop_bytes(&mut st, str_reg, &reg);

    push_u32(&mut st, 0x2); // end memory
    push_u32(&mut st, 0x2); // end root
    push_u32(&mut st, 0x9); // end

    write_header(&mut buf, &st, strings);
    buf
}

/// ```text
/// / {
///     #address-cells = <2>;
///     #size-cells = <1>;
///     memory@0 {
///         device_type = "memory";
///         reg = <0x00 0x00000000 0x40000000>;
///     };
///     reserved-memory {
///         #address-cells = <2>;
///         #size-cells = <1>;
///         ranges;
///         rsv@10000000 {
///             reg = <0x00 0x10000000 0x1000>;
///         };
///     };
///     chosen {
///     };
/// };
/// ```
fn build_dtb_with_children() -> Vec<u8> {
    let mut buf = Vec::new();

    let strings = b"#address-cells\0#size-cells\0device_type\0reg\0ranges\0";
    let str_addr_cells: u32 = 0;
    let str_size_cells: u32 = 15;
    let str_device_type: u32 = 27;
    let str_reg: u32 = 39;
    let str_ranges: u32 = 43;

    let mut st = Vec::new();

    // root
    push_u32(&mut st, 0x1);
    push_u32(&mut st, 0x0);

    push_prop_u32(&mut st, str_addr_cells, 2);
    push_prop_u32(&mut st, str_size_cells, 1);

    // memory@0
    push_u32(&mut st, 0x1);
    st.extend_from_slice(b"memory@0\0");
    align4(&mut st);

    push_prop_bytes(&mut st, str_device_type, b"memory\0");

    let mut reg = Vec::new();
    push_u32(&mut reg, 0x00);
    push_u32(&mut reg, 0x00000000);
    push_u32(&mut reg, 0x40000000);
    push_prop_bytes(&mut st, str_reg, &reg);

    push_u32(&mut st, 0x2); // end memory@0

    // reserved-memory
    push_u32(&mut st, 0x1);
    st.extend_from_slice(b"reserved-memory\0");
    align4(&mut st);

    push_prop_u32(&mut st, str_addr_cells, 2);
    push_prop_u32(&mut st, str_size_cells, 1);
    // ranges (empty value)
    push_u32(&mut st, 0x3);
    push_u32(&mut st, 0);
    push_u32(&mut st, str_ranges);

    // rsv@10000000
    push_u32(&mut st, 0x1);
    st.extend_from_slice(b"rsv@10000000\0");
    align4(&mut st);

    let mut rsv_reg = Vec::new();
    push_u32(&mut rsv_reg, 0x00);
    push_u32(&mut rsv_reg, 0x10000000);
    push_u32(&mut rsv_reg, 0x1000);
    push_prop_bytes(&mut st, str_reg, &rsv_reg);

    push_u32(&mut st, 0x2); // end rsv@10000000
    push_u32(&mut st, 0x2); // end reserved-memory

    // chosen (пустой узел)
    push_u32(&mut st, 0x1);
    st.extend_from_slice(b"chosen\0");
    align4(&mut st);
    push_u32(&mut st, 0x2); // end chosen

    push_u32(&mut st, 0x2); // end root
    push_u32(&mut st, 0x9); // end

    write_header(&mut buf, &st, strings);
    buf
}

fn write_header(buf: &mut Vec<u8>, structure: &[u8], strings: &[u8]) {
    let hdr: u32 = 40;
    let rsvmap: u32 = 16;
    let off_rsvmap = hdr;
    let off_struct = off_rsvmap + rsvmap;
    let off_strings = off_struct + structure.len() as u32;
    let total = off_strings + strings.len() as u32;

    push_u32(buf, 0xD00DFEED);
    push_u32(buf, total);
    push_u32(buf, off_struct);
    push_u32(buf, off_strings);
    push_u32(buf, off_rsvmap);
    push_u32(buf, 17);
    push_u32(buf, 16);
    push_u32(buf, 0);
    push_u32(buf, strings.len() as u32);
    push_u32(buf, structure.len() as u32);

    buf.extend_from_slice(&[0u8; 16]);
    buf.extend_from_slice(structure);
    buf.extend_from_slice(strings);
}

fn push_u32(buf: &mut Vec<u8>, val: u32) {
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

// ─── Тесты ──────────────────────────────────────────────────────────────────

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

    let cells = root.cells_size().unwrap();
    assert_eq!(cells.stride(), 3);
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
        .collect();

    assert_eq!(mem.len(), 1);
    assert_eq!(mem[0].name(), "memory@0");
}

#[test]
fn reg_single_entry() {
    let dtb = build_dtb_with_children();
    let dt = DeviceTree::from_bytes(&dtb).unwrap();
    let root = dt.root().unwrap();
    let cells = root.cells_size().unwrap();

    let mem = root.children().find(|n| n.name() == "memory@0").unwrap();
    let reg = mem.prop("reg").unwrap();
    let list = reg.try_as_reg_list::<8>(cells).unwrap();

    assert_eq!(list.len(), 1);
    assert_eq!(list[0].offset, 0x00);
    assert_eq!(list[0].size, 0x40000000);
}

#[test]
fn find_path() {
    let dtb = build_dtb_with_children();
    let dt = DeviceTree::from_bytes(&dtb).unwrap();

    assert_eq!(dt.find("/").unwrap().name(), "");
    assert_eq!(dt.find("/memory@0").unwrap().name(), "memory@0");
    assert_eq!(
        dt.find("/reserved-memory").unwrap().name(),
        "reserved-memory"
    );
    assert_eq!(dt.find("/chosen").unwrap().name(), "chosen");
    assert!(dt.find("/nonexistent").is_none());
}

#[test]
fn nested_children() {
    let dtb = build_dtb_with_children();
    let dt = DeviceTree::from_bytes(&dtb).unwrap();

    let rsv = dt.find("/reserved-memory").unwrap();
    let children: Vec<_> = rsv.children().collect();
    assert_eq!(children.len(), 1);
    assert_eq!(children[0].name(), "rsv@10000000");

    let cells = rsv.cells_size().unwrap();
    let reg = children[0].prop("reg").unwrap();
    let list = reg.try_as_reg_list::<4>(cells).unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].offset, 0x10000000);
    assert_eq!(list[0].size, 0x1000);
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
    let cells = root.cells_size().unwrap();

    assert_eq!(cells.stride(), 4);

    let mem = root.children().next().unwrap();
    let reg = mem.prop("reg").unwrap();

    assert_eq!(reg.value().len(), 32);

    let list = reg.try_as_reg_list::<8>(cells).unwrap();
    assert_eq!(list.len(), 2);

    assert_eq!(list[0].offset, 0x80000000);
    assert_eq!(list[0].size, 0x80000000);

    assert_eq!(list[1].offset, 0x1_00000000);
    assert_eq!(list[1].size, 0x80000000);
}

#[test]
fn reg_list_n_limits_result() {
    let dtb = build_dtb_two_banks();
    let dt = DeviceTree::from_bytes(&dtb).unwrap();
    let root = dt.root().unwrap();
    let cells = root.cells_size().unwrap();

    let mem = root.children().next().unwrap();
    let reg = mem.prop("reg").unwrap();

    let full = reg.try_as_reg_list::<8>(cells).unwrap();
    assert_eq!(full.len(), 2);

    let limited = reg.try_as_reg_list::<1>(cells).unwrap();
    assert_eq!(limited.len(), 1);
    assert_eq!(limited[0].offset, full[0].offset);
    assert_eq!(limited[0].size, full[0].size);
}
