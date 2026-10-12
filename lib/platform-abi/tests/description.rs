#![allow(clippy::unwrap_used)]

use fdt::devicetreeext::BUS_PATH_MAX_NODES;
use fdt_test::Token;
use platform_abi::{DEVICE_ID_MAX, Description, DescriptionError, DeviceId};

const TREE: &[Token<'static>] = &fdt_test::tree! {
    compatible = "linux,dummy-virt";
    chosen {
        stdout-path = "/pl011@9000000";
    };
    pl031@9010000 {
        compatible = "arm,pl031", "arm,primecell";
        reg = <0x0 0x0901_0000 0x0 0x1000>;
    };
    virtio_mmio@a000000 {
        compatible = "virtio,mmio";
    };
    virtio_mmio@a000200 {
        compatible = "virtio,mmio";
    };
    soc {
        compatible = "simple-bus";
        status = "disabled";
        serial@1000 {
            compatible = "arm,pl011";
        };
    };
    bus {
        uart@2000 {
            compatible = "arm,pl011";
        };
    };
    gpio@4000 {
        compatible = "arm,pl061";
    };
    blank {
        compatible = "";
    };
};

const VALUES: &[Token<'static>] = &[
    Token::Node("values"),
    Token::Cells("one", &[7]),
    Token::Cells("two", &[1, 2]),
    Token::Strings("name", &["pl031"]),
    Token::Bytes("raw", &[1, 2, 3]),
    Token::End,
];

#[test]
fn devices_are_the_available_nodes_with_a_compatible_string_in_description_order() {
    let blob = fdt_test::build(TREE);
    let description = Description::new(&blob, 0).unwrap();

    let devices: Vec<(String, Vec<&str>)> = description
        .devices()
        .map(|device| {
            let device = device.unwrap();
            (
                device.id().as_str().to_owned(),
                device.compatible().collect(),
            )
        })
        .collect();

    assert_eq!(
        devices,
        [
            (
                "/pl031@9010000".to_owned(),
                vec!["arm,pl031", "arm,primecell"]
            ),
            ("/virtio_mmio@a000000".to_owned(), vec!["virtio,mmio"]),
            ("/virtio_mmio@a000200".to_owned(), vec!["virtio,mmio"]),
            ("/bus/uart@2000".to_owned(), vec!["arm,pl011"]),
            ("/gpio@4000".to_owned(), vec!["arm,pl061"]),
        ]
    );
}

#[test]
fn a_refused_node_does_not_stop_the_enumeration() {
    let blob = refused_nodes_before_a_device();
    let description = Description::new(&blob, 0).unwrap();

    let devices: Vec<_> = description
        .devices()
        .map(|device| device.map(|device| device.id().as_str().to_owned()))
        .collect();

    assert_eq!(
        devices,
        [
            Err(DescriptionError::PathTooLong),
            Err(DescriptionError::InvalidValue),
            Ok("/next".to_owned()),
        ]
    );
}

#[test]
fn a_device_past_the_longest_path_is_refused() {
    let mut tokens = Vec::new();
    for depth in 1..=BUS_PATH_MAX_NODES {
        tokens.push(Token::Node("d"));
        if depth >= BUS_PATH_MAX_NODES - 1 {
            tokens.push(Token::Strings("compatible", &["test,device"]));
        }
    }
    tokens.extend((0..BUS_PATH_MAX_NODES).map(|_| Token::End));
    let blob = fdt_test::build(&tokens);
    let description = Description::new(&blob, 0).unwrap();

    let devices: Vec<_> = description
        .devices()
        .map(|device| device.map(|device| device.id().as_str().to_owned()))
        .collect();

    assert_eq!(
        devices,
        [
            Ok("/d".repeat(BUS_PATH_MAX_NODES - 1)),
            Err(DescriptionError::PathTooLong),
        ]
    );
}

#[test]
fn any_node_is_found_regardless_of_its_status() {
    let blob = fdt_test::build(TREE);
    let description = Description::new(&blob, 0).unwrap();

    let serial = description.find("/soc/serial@1000");
    let chosen = description.find("/chosen");
    let absent = description.find("/absent");

    assert_eq!(
        serial
            .unwrap()
            .strings("compatible")
            .unwrap()
            .map(Iterator::collect::<Vec<_>>),
        Some(vec!["arm,pl011"])
    );
    assert_eq!(
        chosen.unwrap().string("stdout-path"),
        Ok(Some("/pl011@9000000"))
    );
    assert!(absent.is_none());
}

#[test]
fn a_device_node_is_found_by_its_id_only_when_available() {
    let blob = fdt_test::build(TREE);
    let description = Description::new(&blob, 0).unwrap();
    let available = DeviceId::new("/pl031@9010000").unwrap();
    let disabled = DeviceId::new("/soc/serial@1000").unwrap();

    let available = description.node(&available);
    let disabled = description.node(&disabled);

    assert!(available.is_some());
    assert!(disabled.is_none());
}

#[test]
fn a_property_is_read_in_the_requested_form() {
    let blob = fdt_test::build(VALUES);
    let description = Description::new(&blob, 0).unwrap();
    let node = description.find("/values").unwrap();

    let one = node.u32("one");
    let two = node.u64("two");
    let name = node.string("name");
    let names = node.strings("name");
    let raw = node.bytes("raw");
    let cells = node.cells("two");
    let absent = node.u32("absent");
    let absent_cells = node.cells("absent");

    assert_eq!(one, Ok(Some(7)));
    assert_eq!(two, Ok(Some(0x1_0000_0002)));
    assert_eq!(name, Ok(Some("pl031")));
    assert_eq!(
        names.unwrap().map(Iterator::collect::<Vec<_>>),
        Some(vec!["pl031"])
    );
    assert_eq!(raw, Some(&[1, 2, 3][..]));
    assert_eq!(
        cells.unwrap().map(Iterator::collect::<Vec<_>>),
        Some(vec![1, 2])
    );
    assert_eq!(absent, Ok(None));
    assert!(matches!(absent_cells, Ok(None)));
}

#[test]
fn a_property_of_another_length_or_form_is_invalid() {
    let blob = fdt_test::build(VALUES);
    let description = Description::new(&blob, 0).unwrap();
    let node = description.find("/values").unwrap();

    let two = node.u32("two");
    let one = node.u64("one");
    let raw = node.string("raw");
    let raw_list = node.strings("raw");
    let raw_cells = node.cells("raw");

    assert_eq!(two, Err(DescriptionError::InvalidValue));
    assert_eq!(one, Err(DescriptionError::InvalidValue));
    assert_eq!(raw, Err(DescriptionError::InvalidValue));
    assert!(matches!(raw_list, Err(DescriptionError::InvalidValue)));
    assert!(matches!(raw_cells, Err(DescriptionError::InvalidValue)));
}

#[test]
fn ancestors_go_from_the_parent_up_to_a_child_of_the_root_regardless_of_status() {
    let blob = fdt_test::build(TREE);
    let description = Description::new(&blob, 0).unwrap();
    let ancestors = |path| {
        description
            .ancestors(&DeviceId::new(path).unwrap())
            .map(|ancestor| {
                (
                    ancestor.id.as_str().to_owned(),
                    ancestor.node.bytes("compatible"),
                )
            })
            .collect::<Vec<_>>()
    };

    assert_eq!(ancestors("/bus/uart@2000"), [("/bus".to_owned(), None)]);
    assert_eq!(
        ancestors("/soc/serial@1000"),
        [("/soc".to_owned(), Some(&b"simple-bus\0"[..]))]
    );
    assert!(ancestors("/bus").is_empty());
    assert!(ancestors("/absent/node").is_empty());
}

#[test]
fn children_are_the_available_child_nodes_with_their_paths() {
    let tokens = [
        Token::Node("keys"),
        Token::Node("up"),
        Token::End,
        Token::Node("off"),
        Token::Strings("status", &["disabled"]),
        Token::End,
        Token::Node("down@1"),
        Token::End,
        Token::End,
        Token::Node("hidden"),
        Token::Strings("status", &["disabled"]),
        Token::Node("inner"),
        Token::End,
        Token::End,
    ];
    let blob = fdt_test::build(&tokens);
    let description = Description::new(&blob, 0).unwrap();
    let children = |path| {
        description
            .children(&DeviceId::new(path).unwrap())
            .map(|child| child.map(|child| child.id.as_str().to_owned()))
            .collect::<Vec<_>>()
    };

    assert_eq!(
        children("/keys"),
        [Ok("/keys/up".to_owned()), Ok("/keys/down@1".to_owned())]
    );
    assert!(children("/hidden").is_empty());
    assert!(children("/absent").is_empty());
}

#[test]
fn a_child_past_the_longest_path_is_refused() {
    let mut tokens = vec![Token::Node("d"); BUS_PATH_MAX_NODES];
    tokens.extend((0..BUS_PATH_MAX_NODES).map(|_| Token::End));
    let blob = fdt_test::build(&tokens);
    let description = Description::new(&blob, 0).unwrap();
    let children = |depth| {
        description
            .children(&DeviceId::new(&"/d".repeat(depth)).unwrap())
            .map(|child| child.map(|child| child.id.as_str().to_owned()))
            .collect::<Vec<_>>()
    };

    assert_eq!(
        children(BUS_PATH_MAX_NODES - 2),
        [Ok("/d".repeat(BUS_PATH_MAX_NODES - 1))]
    );
    assert_eq!(
        children(BUS_PATH_MAX_NODES - 1),
        [Err(DescriptionError::PathTooLong)]
    );
}

#[test]
fn an_ancestor_path_equals_the_same_path_given_as_an_id() {
    let tokens = [
        Token::Node("a"),
        Token::Node("b"),
        Token::Node("c"),
        Token::End,
        Token::End,
        Token::End,
    ];
    let blob = fdt_test::build(&tokens);
    let description = Description::new(&blob, 0).unwrap();

    let ancestors: Vec<_> = description
        .ancestors(&DeviceId::new("/a/b/c").unwrap())
        .map(|ancestor| ancestor.id)
        .collect();

    assert_eq!(
        ancestors,
        [DeviceId::new("/a/b").unwrap(), DeviceId::new("/a").unwrap()]
    );
}

#[test]
fn a_description_is_read_at_its_offset_within_the_region() {
    let blob = fdt_test::build(TREE);
    let mut region = vec![0xff; 24];
    region.extend_from_slice(&blob);

    let description = Description::new(&region, 24);

    assert!(description.unwrap().find("/chosen").is_some());
}

#[test]
fn a_description_past_the_region_or_with_a_broken_structure_is_malformed() {
    let blob = fdt_test::build(TREE);
    let structure = u32::from_be_bytes(blob[8..12].try_into().unwrap()) as usize;
    let mut broken = blob.clone();
    broken[structure..structure + 4].copy_from_slice(&7u32.to_be_bytes());

    let descriptions = [
        Description::new(&blob[..blob.len() - 1], 0),
        Description::new(&blob, 1),
        Description::new(&broken, 0),
    ];

    for description in descriptions {
        assert!(matches!(description, Err(DescriptionError::Malformed)));
    }
}

#[test]
fn compatible_devices_match_any_string_of_an_available_node() {
    let blob = fdt_test::build(TREE);
    let description = Description::new(&blob, 0).unwrap();
    let ids = |compatible| -> Vec<String> {
        description
            .compatible_devices(compatible)
            .map(|device| device.id().as_str().to_owned())
            .collect()
    };

    assert_eq!(
        ids("virtio,mmio"),
        ["/virtio_mmio@a000000", "/virtio_mmio@a000200"]
    );
    assert_eq!(ids("arm,primecell"), ["/pl031@9010000"]);
    assert_eq!(ids("arm,pl011"), ["/bus/uart@2000"]);
    assert!(ids("arm,pl050").is_empty());
}

#[test]
fn compatible_devices_skip_a_refused_node_before_a_match() {
    let blob = refused_nodes_before_a_device();
    let description = Description::new(&blob, 0).unwrap();

    let ids: Vec<_> = description
        .compatible_devices("test,device")
        .map(|device| device.id().as_str().to_owned())
        .collect();

    assert_eq!(ids, ["/next"]);
}

fn refused_nodes_before_a_device() -> Vec<u8> {
    let long = "n".repeat(DEVICE_ID_MAX);
    fdt_test::build(&[
        Token::Node(&long),
        Token::Strings("compatible", &["test,device"]),
        Token::Node("child"),
        Token::Strings("compatible", &["test,device"]),
        Token::End,
        Token::End,
        Token::Node("broken"),
        Token::Bytes("compatible", b"test,device"),
        Token::End,
        Token::Node("next"),
        Token::Strings("compatible", &["test,device"]),
        Token::End,
    ])
}
