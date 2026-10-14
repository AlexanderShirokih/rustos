#![allow(clippy::unwrap_used)]

use fdt_test::Token;
use pins_abi::{interrupt, line};
use platform_abi::{Description, DeviceId};

const TREE: &[Token<'static>] = &fdt_test::tree! {
    soc@0 {
        gpio@1000 {
            #gpio-cells = <2>;
            phandle = <1>;
        };
        gpio@2000 {
            #gpio-cells = <2>;
            #interrupt-cells = <2>;
            phandle = <2>;
        };
    };
    gpio-keys {
        key-up {
            gpios = <1 7 1 2 3 0>;
            reset-gpios = <2 4 1>;
            interrupts-extended = <2 5 1 2 6 8>;
        };
    };
    gpio-keys-extra {
        gpios = <1 8 0>;
        interrupts-extended = <2 9 1>;
    };
};

fn record(node: &str, name: &str, index: u8) -> Option<(String, Vec<u32>)> {
    let blob = fdt_test::build(TREE);
    let description = Description::new(&blob, 0).ok()?;
    let session = DeviceId::new("/gpio-keys")?;
    let node = DeviceId::new(node)?;

    line(&description, &session, &node, name, index)
        .map(|record| (record.controller.as_str().into(), record.cells))
}

fn interrupt_record(node: &str, index: u8) -> Option<(String, Vec<u32>)> {
    let blob = fdt_test::build(TREE);
    let description = Description::new(&blob, 0).ok()?;
    let session = DeviceId::new("/gpio-keys")?;
    let node = DeviceId::new(node)?;

    interrupt(&description, &session, &node, index)
        .map(|record| (record.controller.as_str().into(), record.cells))
}

#[test]
fn a_named_line_takes_its_gpios_property_and_an_index_past_the_records_names_no_line() {
    assert_eq!(
        record("/gpio-keys/key-up", "reset", 0),
        Some(("/soc@0/gpio@2000".into(), vec![4, 1]))
    );
    assert_eq!(
        record("/gpio-keys/key-up", "", 1),
        Some(("/soc@0/gpio@2000".into(), vec![3, 0]))
    );
    assert_eq!(record("/gpio-keys/key-up", "", 2), None);
    assert_eq!(record("/gpio-keys/key-up", "enable", 0), None);
}

#[test]
fn a_node_outside_the_session_names_no_line() {
    assert_eq!(record("/gpio-keys-extra", "", 0), None);
}

#[test]
fn an_interrupt_record_takes_the_interrupt_cells_of_its_controller() {
    assert_eq!(
        interrupt_record("/gpio-keys/key-up", 1),
        Some(("/soc@0/gpio@2000".into(), vec![6, 8]))
    );
    assert_eq!(interrupt_record("/gpio-keys/key-up", 2), None);
    assert_eq!(interrupt_record("/gpio-keys", 0), None);
    assert_eq!(interrupt_record("/gpio-keys-extra", 0), None);
}
