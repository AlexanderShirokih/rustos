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
    use core::slice;

    use fdt::devicetree::DeviceTree;
    use fdt_test::Token::{self, Bytes, Cells, End, Node, Strings};

    use super::{designated_console, find_initrd_payload};

    const INITRD_START: &str = "linux,initrd-start";
    const INITRD_END: &str = "linux,initrd-end";

    fn build_chosen_dtb(properties: &[Token<'_>]) -> Vec<u8> {
        let mut tokens = vec![Node("chosen")];
        tokens.extend_from_slice(properties);
        tokens.push(End);
        fdt_test::build(&tokens)
    }

    fn build_initrd_dtb(start: u64, end: u64) -> Vec<u8> {
        build_chosen_dtb(&[
            Bytes(INITRD_START, &start.to_be_bytes()),
            Bytes(INITRD_END, &end.to_be_bytes()),
        ])
    }

    #[test]
    fn finds_initrd_payload_in_chosen() {
        let bytes = build_initrd_dtb(0x0000_0000_c800_0000, 0x0000_0000_c820_0000);
        let tree = DeviceTree::from_bytes(&bytes).unwrap();
        let payload = find_initrd_payload(&tree).unwrap();

        assert_eq!(payload.start().as_usize(), 0xc800_0000);
        assert_eq!(payload.size_bytes(), 0x20_0000);
        assert_eq!(payload.end_exclusive().as_usize(), 0xc820_0000);
    }

    #[test]
    fn finds_initrd_payload_in_32bit_chosen_properties() {
        let bytes = build_chosen_dtb(&[
            Cells(INITRD_START, &[0xc800_0000]),
            Cells(INITRD_END, &[0xc820_0000]),
        ]);
        let tree = DeviceTree::from_bytes(&bytes).unwrap();
        let payload = find_initrd_payload(&tree).unwrap();

        assert_eq!(payload.start().as_usize(), 0xc800_0000);
        assert_eq!(payload.size_bytes(), 0x20_0000);
        assert_eq!(payload.end_exclusive().as_usize(), 0xc820_0000);
    }

    #[test]
    fn returns_none_when_payload_is_missing() {
        let bytes = build_chosen_dtb(&[]);
        let tree = DeviceTree::from_bytes(&bytes).unwrap();

        assert!(find_initrd_payload(&tree).is_none());
    }

    #[test]
    fn returns_none_when_only_one_initrd_property_is_present() {
        let only_start = build_chosen_dtb(&[Bytes(INITRD_START, &0x1000u64.to_be_bytes())]);
        let only_end = build_chosen_dtb(&[Bytes(INITRD_END, &0x2000u64.to_be_bytes())]);

        assert!(find_initrd_payload(&DeviceTree::from_bytes(&only_start).unwrap()).is_none());
        assert!(find_initrd_payload(&DeviceTree::from_bytes(&only_end).unwrap()).is_none());
    }

    #[test]
    fn returns_none_for_malformed_initrd_properties() {
        let malformed = build_chosen_dtb(&[
            Bytes(INITRD_START, &[0x12, 0x34]),
            Bytes(INITRD_END, &[0x56]),
        ]);

        assert!(find_initrd_payload(&DeviceTree::from_bytes(&malformed).unwrap()).is_none());
    }

    #[test]
    fn returns_none_for_empty_or_reversed_ranges() {
        let empty = build_initrd_dtb(0x1000, 0x1000);
        let reversed = build_initrd_dtb(0x2000, 0x1000);

        assert!(find_initrd_payload(&DeviceTree::from_bytes(&empty).unwrap()).is_none());
        assert!(find_initrd_payload(&DeviceTree::from_bytes(&reversed).unwrap()).is_none());
    }

    fn build_console_dtb(stdout_path: Option<&str>) -> Vec<u8> {
        let mut tokens = vec![
            Cells("#address-cells", &[2]),
            Cells("#size-cells", &[1]),
            Node("aliases"),
            Strings("serial0", &["/soc/serial@2000"]),
            End,
        ];
        if let Some(path) = &stdout_path {
            tokens.extend([
                Node("chosen"),
                Strings("stdout-path", slice::from_ref(path)),
                End,
            ]);
        }
        tokens.extend([
            Node("soc"),
            Node("serial@2000"),
            Strings("device_type", &["serial"]),
            Strings("compatible", &["arm,pl011"]),
            End,
            End,
        ]);
        fdt_test::build(&tokens)
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
