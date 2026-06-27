//! Перечисление MMIO-окон device-узлов FDT для выдачи userspace-драйверам.

use alloc::{collections::BTreeSet, vec::Vec};

use drivers_common::DeviceNode;

use crate::{
    fdt_adapter::FdtNode,
    tree_ext::{AddressSpace, NodeAddressExt},
};

/// Перечисляет MMIO-окна прямых device-детей `root`, отбрасывая узлы, чей `id`
/// входит в `claimed` (kernel-owned: ядро подобрало под них драйвер при probe).
/// Узлы без `compatible` (memory, cpus, chosen) и без `reg` окон не дают. Окна
/// идут в порядке узлов FDT и их `reg`-записей.
///
/// Обходятся только прямые дети корня (плоская раскладка); узлы под шиной с
/// собственным `ranges` в набор не попадают.
pub fn enumerate_device_windows(root: &FdtNode, claimed: &BTreeSet<usize>) -> Vec<AddressSpace> {
    let root_cells = root.cells_size().unwrap_or_default();
    let mut windows = Vec::new();
    for child in root.children() {
        if child.prop("compatible").is_none() || claimed.contains(&child.id()) {
            continue;
        }
        windows.extend(child.reg_iter(root_cells).filter(|window| window.size > 0));
    }
    windows
}

#[cfg(test)]
mod tests {
    extern crate std;

    use std::vec::Vec;

    use drivers_common::DeviceNode;
    use fdt::devicetree::DeviceTree;

    use super::*;
    use crate::adapt_to_fdt_tree;

    fn push_u32(buf: &mut Vec<u8>, value: u32) {
        buf.extend_from_slice(&value.to_be_bytes());
    }

    fn align4(buf: &mut Vec<u8>) {
        while !buf.len().is_multiple_of(4) {
            buf.push(0);
        }
    }

    /// Дописывает property с сырыми байтами; `name_off` - смещение имени в
    /// блоке строк.
    fn push_prop(structure: &mut Vec<u8>, name_off: u32, value: &[u8]) {
        push_u32(structure, 0x3);
        push_u32(structure, value.len() as u32);
        push_u32(structure, name_off);
        structure.extend_from_slice(value);
        align4(structure);
    }

    fn push_node_begin(structure: &mut Vec<u8>, name: &str) {
        push_u32(structure, 0x1);
        structure.extend_from_slice(name.as_bytes());
        structure.push(0);
        align4(structure);
    }

    fn push_node_end(structure: &mut Vec<u8>) {
        push_u32(structure, 0x2);
    }

    /// reg-запись для root с `#address-cells = 2`, `#size-cells = 2`.
    fn reg_2_2(base: u64, size: u64) -> Vec<u8> {
        let mut reg = Vec::new();
        push_u32(&mut reg, (base >> 32) as u32);
        push_u32(&mut reg, base as u32);
        push_u32(&mut reg, (size >> 32) as u32);
        push_u32(&mut reg, size as u32);
        reg
    }

    fn wrap_dtb(structure: &[u8], strings: &[u8]) -> Vec<u8> {
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
    /// / { #address-cells = <2>; #size-cells = <2>;
    ///     pl011@9000000 { compatible = "arm,pl011"; reg = <0 0x9000000 0 0x1000>; };
    ///     rtc@9010000   { compatible = "arm,pl031"; reg = <0 0x9010000 0 0x1000>; };
    ///     memory@40000000 { device_type = "memory"; reg = <0 0x40000000 0 0x1000>; };
    /// };
    /// ```
    fn build_dtb() -> Vec<u8> {
        let strings = b"#address-cells\0#size-cells\0compatible\0reg\0device_type\0";
        let s_addr: u32 = 0;
        let s_size: u32 = 15;
        let s_compat: u32 = 27;
        let s_reg: u32 = 38;
        let s_devtype: u32 = 42;

        let mut st = Vec::new();
        push_node_begin(&mut st, "");
        push_prop(&mut st, s_addr, &2u32.to_be_bytes());
        push_prop(&mut st, s_size, &2u32.to_be_bytes());

        push_node_begin(&mut st, "pl011@9000000");
        push_prop(&mut st, s_compat, b"arm,pl011\0");
        push_prop(&mut st, s_reg, &reg_2_2(0x0900_0000, 0x1000));
        push_node_end(&mut st);

        push_node_begin(&mut st, "rtc@9010000");
        push_prop(&mut st, s_compat, b"arm,pl031\0");
        push_prop(&mut st, s_reg, &reg_2_2(0x0901_0000, 0x1000));
        push_node_end(&mut st);

        push_node_begin(&mut st, "memory@40000000");
        push_prop(&mut st, s_devtype, b"memory\0");
        push_prop(&mut st, s_reg, &reg_2_2(0x4000_0000, 0x1000));
        push_node_end(&mut st);

        push_node_end(&mut st);
        push_u32(&mut st, 0x9);

        wrap_dtb(&st, strings)
    }

    #[test]
    fn excludes_claimed_nodes_and_nodes_without_compatible() {
        let dtb = build_dtb();
        let tree = DeviceTree::from_ptr(dtb.as_ptr() as usize).expect("valid dtb");
        let root = adapt_to_fdt_tree(&tree).root().expect("root node");

        // Имитируем claim ядром узла pl011 (как сделал бы probe-проход).
        let pl011_id = root
            .children()
            .find(|child| child.name().starts_with("pl011"))
            .expect("pl011 node present")
            .id();
        let claimed = BTreeSet::from([pl011_id]);

        let windows = enumerate_device_windows(&root, &claimed);

        assert_eq!(windows.len(), 1, "claimed node and memory@ excluded");
        assert_eq!(windows[0].offset, 0x0901_0000);
        assert_eq!(windows[0].size, 0x1000);
    }
}
