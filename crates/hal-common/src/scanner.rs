use fdt::devicetree::{DeviceTree, Node};
use memory::physical_address::PhysicalAddress;

use crate::boot::BootPayloadRange;

/// Находит узел консоли из stdout-path или первый serial.
pub fn find_console<'dt>(device_tree: &'dt DeviceTree<'dt>) -> Option<Node<'dt>> {
    stdout_node(device_tree).or_else(|| first_serial(device_tree))
}

/// Извлекает initial ramdisk из `/chosen` (linux-compatible протокол).
pub fn find_initrd_payload(device_tree: &DeviceTree<'_>) -> Option<BootPayloadRange> {
    let chosen = device_tree.find("/chosen")?;
    let start = chosen.prop("linux,initrd-start")?.as_usize();
    let end = chosen.prop("linux,initrd-end")?.as_usize();
    let size_bytes = end.checked_sub(start)?;

    BootPayloadRange::new(PhysicalAddress::new(start), size_bytes)
}

fn stdout_node<'dt>(device_tree: &'dt DeviceTree<'dt>) -> Option<Node<'dt>> {
    let chosen = device_tree.find("/chosen")?;
    let raw = chosen.prop("stdout-path")?;
    let path = raw.as_cstr()?;
    let trimmed = path.split(':').next().unwrap_or(path);
    device_tree.find(trimmed)
}

fn first_serial<'dt>(device_tree: &'dt DeviceTree<'dt>) -> Option<Node<'dt>> {
    device_tree.nodes().find(|node| {
        let by_device_type = node
            .prop("device_type")
            .and_then(|prop| prop.as_cstr())
            .is_some_and(|value| value == "serial");

        let by_name = node.name().starts_with("serial");
        let by_compat = compatible_strings(node)
            .any(|value| value.contains("serial") || value.contains("uart"));

        by_device_type || by_name || by_compat
    })
}

/// Итерирует по null-terminated строкам из свойства `compatible`.
fn compatible_strings<'a>(node: &'a Node<'a>) -> impl Iterator<Item = &'a str> {
    let data = node.prop("compatible").map_or(&[] as &[u8], |p| p.value());

    data.split(|&b| b == 0)
        .filter(|s| !s.is_empty())
        .filter_map(|bytes| core::str::from_utf8(bytes).ok())
}

#[cfg(test)]
mod tests {
    use fdt::devicetree::DeviceTree;

    use super::find_initrd_payload;

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

    fn build_chosen_dtb(initrd: Option<(u64, u64)>) -> Vec<u8> {
        build_chosen_dtb_with_payload(
            initrd.map(|(start, end)| (start.to_be_bytes().to_vec(), end.to_be_bytes().to_vec())),
        )
    }

    fn build_chosen_dtb_32(initrd: Option<(u32, u32)>) -> Vec<u8> {
        build_chosen_dtb_with_payload(
            initrd.map(|(start, end)| (start.to_be_bytes().to_vec(), end.to_be_bytes().to_vec())),
        )
    }

    fn build_chosen_dtb_with_payload(initrd: Option<(Vec<u8>, Vec<u8>)>) -> Vec<u8> {
        let strings = b"linux,initrd-start\0linux,initrd-end\0";
        let start_off = 0u32;
        let end_off = 19u32;

        let mut structure = Vec::new();
        push_u32(&mut structure, 0x1);
        push_u32(&mut structure, 0x0);

        push_u32(&mut structure, 0x1);
        structure.extend_from_slice(b"chosen\0");
        align4(&mut structure);

        if let Some((start, end)) = initrd {
            if !start.is_empty() {
                push_prop_bytes(&mut structure, start_off, &start);
            }
            if !end.is_empty() {
                push_prop_bytes(&mut structure, end_off, &end);
            }
        }

        push_u32(&mut structure, 0x2);
        push_u32(&mut structure, 0x2);
        push_u32(&mut structure, 0x9);

        build_fdt(structure, strings)
    }

    fn build_fdt(structure: Vec<u8>, strings: &[u8]) -> Vec<u8> {
        let off_mem_rsvmap = 40u32;
        let off_struct = off_mem_rsvmap + 16;
        let off_strings = off_struct + structure.len() as u32;
        let total = off_strings + strings.len() as u32;

        let mut buf = Vec::new();
        push_u32(&mut buf, 0xD00D_FEED);
        push_u32(&mut buf, total);
        push_u32(&mut buf, off_struct);
        push_u32(&mut buf, off_strings);
        push_u32(&mut buf, off_mem_rsvmap);
        push_u32(&mut buf, 17);
        push_u32(&mut buf, 16);
        push_u32(&mut buf, 0);
        push_u32(&mut buf, strings.len() as u32);
        push_u32(&mut buf, structure.len() as u32);

        buf.extend_from_slice(&[0; 16]);
        buf.extend_from_slice(&structure);
        buf.extend_from_slice(strings);
        buf
    }

    fn push_u32(buf: &mut Vec<u8>, value: u32) {
        buf.extend_from_slice(&value.to_be_bytes());
    }

    fn push_prop_bytes(buf: &mut Vec<u8>, name_off: u32, value: &[u8]) {
        push_u32(buf, 0x3);
        push_u32(buf, value.len() as u32);
        push_u32(buf, name_off);
        buf.extend_from_slice(value);
        align4(buf);
    }

    fn align4(buf: &mut Vec<u8>) {
        while !buf.len().is_multiple_of(4) {
            buf.push(0);
        }
    }
}
