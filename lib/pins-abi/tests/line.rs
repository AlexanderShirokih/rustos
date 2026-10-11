#![allow(clippy::unwrap_used)]

use fdt_test::Token;
use pins_abi::line;
use platform_abi::{Description, DeviceId};

const TREE: &[Token<'static>] = &fdt_test::tree! {
    soc@0 {
        gpio@1000 {
            #gpio-cells = <2>;
            phandle = <1>;
        };
        gpio@2000 {
            #gpio-cells = <2>;
            phandle = <2>;
        };
    };
    gpio-keys {
        key-up {
            gpios = <1 7 1 2 3 0>;
            lines = <1 7 1>;
        };
    };
};

fn record(property: &str, index: u8) -> Option<(String, Vec<u32>)> {
    let blob = fdt_test::build(TREE);
    let description = Description::new(&blob, 0).ok()?;
    let node = DeviceId::new("/gpio-keys/key-up")?;

    line(&description, &node, property, index)
        .map(|record| (record.controller.as_str().into(), record.cells))
}

#[test]
fn a_property_outside_the_gpios_names_and_an_index_past_the_records_name_no_line() {
    assert_eq!(record("lines", 0), None);
    assert_eq!(record("gpios", 2), None);
    assert_eq!(
        record("gpios", 1),
        Some(("/soc@0/gpio@2000".into(), vec![3, 0]))
    );
}
