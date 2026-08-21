use fdt::devicetree::{DeviceTree, Node};
use memory::physical_address::PhysicalAddress;

use crate::boot::BootPayloadRange;

/// Возвращает узел, назначенный загрузчиком консолью в `/chosen/stdout-path`.
/// `None` - назначения нет либо путь не разрешается в узел.
pub fn designated_console<'dt>(device_tree: &'dt DeviceTree<'dt>) -> Option<Node<'dt>> {
    let chosen = device_tree.find("/chosen")?;
    let raw = chosen.prop("stdout-path")?;
    let path = raw.as_cstr()?;
    // Суффикс за ':' - параметры линии, узла он не называет.
    let trimmed = path.split(':').next().unwrap_or(path);
    device_tree.find(trimmed)
}

/// Извлекает initial ramdisk из `/chosen` (linux-compatible протокол).
pub fn find_initrd_payload(device_tree: &DeviceTree<'_>) -> Option<BootPayloadRange> {
    let chosen = device_tree.find("/chosen")?;
    let start = chosen.prop("linux,initrd-start")?.as_usize();
    let end = chosen.prop("linux,initrd-end")?.as_usize();
    let size_bytes = end.checked_sub(start)?;

    BootPayloadRange::new(PhysicalAddress::new(start), size_bytes)
}

#[cfg(test)]
mod tests {
    use fdt::devicetree::DeviceTree;

    use super::{designated_console, find_initrd_payload};
    use crate::test_util::{
        StringPool, begin_node, build_chosen_dtb, build_chosen_dtb_32,
        build_chosen_dtb_with_payload, build_fdt, end_node, end_tree, push_prop_bytes,
    };

    #[test]
    fn finds_initrd_payload_in_chosen() {
        let bytes = build_chosen_dtb(Some((0x0000_0000_c800_0000, 0x0000_0000_c820_0000)));
        let tree = DeviceTree::from_bytes(&bytes).unwrap();
        let payload = find_initrd_payload(&tree).unwrap();

        assert_eq!(payload.start().as_usize(), 0xc800_0000);
        assert_eq!(payload.size_bytes(), 0x20_0000);
        assert_eq!(payload.end_exclusive().as_usize(), 0xc820_0000);
    }

    #[test]
    fn finds_initrd_payload_in_32bit_chosen_properties() {
        let bytes = build_chosen_dtb_32(Some((0xc800_0000, 0xc820_0000)));
        let tree = DeviceTree::from_bytes(&bytes).unwrap();
        let payload = find_initrd_payload(&tree).unwrap();

        assert_eq!(payload.start().as_usize(), 0xc800_0000);
        assert_eq!(payload.size_bytes(), 0x20_0000);
        assert_eq!(payload.end_exclusive().as_usize(), 0xc820_0000);
    }

    #[test]
    fn returns_none_when_payload_is_missing() {
        let bytes = build_chosen_dtb(None);
        let tree = DeviceTree::from_bytes(&bytes).unwrap();

        assert!(find_initrd_payload(&tree).is_none());
    }

    #[test]
    fn returns_none_when_only_one_initrd_property_is_present() {
        let only_start =
            build_chosen_dtb_with_payload(Some((0x1000u64.to_be_bytes().to_vec(), vec![])));
        let only_end =
            build_chosen_dtb_with_payload(Some((vec![], 0x2000u64.to_be_bytes().to_vec())));

        assert!(find_initrd_payload(&DeviceTree::from_bytes(&only_start).unwrap()).is_none());
        assert!(find_initrd_payload(&DeviceTree::from_bytes(&only_end).unwrap()).is_none());
    }

    #[test]
    fn returns_none_for_malformed_initrd_properties() {
        let malformed = build_chosen_dtb_with_payload(Some((vec![0x12, 0x34], vec![0x56])));

        assert!(find_initrd_payload(&DeviceTree::from_bytes(&malformed).unwrap()).is_none());
    }

    #[test]
    fn returns_none_for_empty_or_reversed_ranges() {
        let empty = build_chosen_dtb(Some((0x1000, 0x1000)));
        let reversed = build_chosen_dtb(Some((0x2000, 0x1000)));

        assert!(find_initrd_payload(&DeviceTree::from_bytes(&empty).unwrap()).is_none());
        assert!(find_initrd_payload(&DeviceTree::from_bytes(&reversed).unwrap()).is_none());
    }

    // --- Console / serial search ---

    fn build_console_dtb(stdout_path: Option<&str>) -> Vec<u8> {
        let mut pool = StringPool::new();
        let s_addr = pool.intern("#address-cells");
        let s_size = pool.intern("#size-cells");
        let s_compat = pool.intern("compatible");
        let s_devtype = pool.intern("device_type");
        let s_serial0 = pool.intern("serial0");
        let s_stdout = pool.intern("stdout-path");

        let mut st = Vec::new();
        begin_node(&mut st, "");
        push_prop_bytes(&mut st, s_addr, &2u32.to_be_bytes());
        push_prop_bytes(&mut st, s_size, &1u32.to_be_bytes());

        // aliases { serial0 = "/soc/serial@2000"; }
        begin_node(&mut st, "aliases");
        push_prop_bytes(&mut st, s_serial0, b"/soc/serial@2000\0");
        end_node(&mut st);

        // chosen { stdout-path = ... } (опционально)
        if let Some(path) = stdout_path {
            begin_node(&mut st, "chosen");
            let mut val = path.as_bytes().to_vec();
            val.push(0);
            push_prop_bytes(&mut st, s_stdout, &val);
            end_node(&mut st);
        }

        // soc { serial@2000 { device_type="serial"; compatible="arm,pl011"; } }
        begin_node(&mut st, "soc");
        begin_node(&mut st, "serial@2000");
        push_prop_bytes(&mut st, s_devtype, b"serial\0");
        push_prop_bytes(&mut st, s_compat, b"arm,pl011\0");
        end_node(&mut st); // serial
        end_node(&mut st); // soc

        end_node(&mut st); // root
        end_tree(&mut st);

        build_fdt(&st, pool.bytes())
    }

    #[test]
    fn designation_resolves_a_full_path() {
        let dtb = build_console_dtb(Some("/soc/serial@2000:115200n8"));
        let tree = DeviceTree::from_bytes(&dtb).unwrap();

        // stdout-path с :-суффиксом должен резолвиться в узел.
        let console = designated_console(&tree).expect("stdout-path");
        assert_eq!(console.name(), "serial@2000");
    }

    #[test]
    fn designation_resolves_an_alias() {
        // Форма Raspberry Pi: stdout-path называет алиас, а не путь.
        let dtb = build_console_dtb(Some("serial0:115200n8"));
        let tree = DeviceTree::from_bytes(&dtb).unwrap();

        let console = designated_console(&tree).expect("alias stdout-path");
        assert_eq!(console.name(), "serial@2000");
    }

    #[test]
    fn designation_is_absent_without_stdout_path() {
        let dtb = build_console_dtb(None);
        let tree = DeviceTree::from_bytes(&dtb).unwrap();

        assert!(designated_console(&tree).is_none());
    }

    #[test]
    fn designation_is_absent_when_the_path_names_no_node() {
        let dtb = build_console_dtb(Some("/soc/serial@9999"));
        let tree = DeviceTree::from_bytes(&dtb).unwrap();

        assert!(designated_console(&tree).is_none());
    }
}
