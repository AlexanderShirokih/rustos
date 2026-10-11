#![allow(clippy::unwrap_used)]

use fdt_test::Token;
use pins_abi::{controllers, default_states};
use platform_abi::{Description, Node};

const TREE: &[Token<'static>] = &fdt_test::tree! {
    soc@0 {
        pinctrl@3100000 {
            compatible = "test,pinctrl";
            uart-default {
                function = "uart";
                phandle = <1>;
            };
            i2c-default {
                function = "i2c";
                phandle = <2>;
            };
            spi-default {
                function = "spi";
                phandle = <3>;
            };
        };
        spmi@800f000 {
            compatible = "qcom,spmi-pmic-arb";
            pmic@2 {
                compatible = "qcom,pm660l", "qcom,spmi-pmic";
                gpio@c000 {
                    compatible = "qcom,pm660l-gpio", "qcom,spmi-gpio";
                    key-default {
                function = "key";
                        phandle = <4>;
                    };
                };
            };
        };
        mixed@1000 {
            compatible = "test,mixed";
            pinctrl-names = "default";
            "pinctrl-0" = <1 4 2>;
        };
        spi@2000 {
            compatible = "test,spi";
            pinctrl-names = "sleep", "default";
            "pinctrl-0" = <1>;
            "pinctrl-1" = <3>;
        };
        plain@3000 {
            compatible = "test,plain";
        };
        dangling@4000 {
            compatible = "test,dangling";
            pinctrl-names = "default";
            "pinctrl-0" = <1 9>;
        };
    };
};

fn with_description<T>(inspect: impl FnOnce(&Description<'_>) -> T) -> T {
    let blob = fdt_test::build(TREE);
    inspect(&Description::new(&blob, 0).unwrap())
}

fn text(node: &Node<'_>, name: &str) -> String {
    node.string(name).unwrap().unwrap().to_owned()
}

fn controllers_of(device: &str) -> Option<Vec<String>> {
    with_description(|description| {
        controllers(description, device)
            .map(|found| found.iter().map(|node| text(node, "compatible")).collect())
    })
}

fn functions_of(device: &str) -> Option<Vec<String>> {
    with_description(|description| {
        default_states(description, device).map(|states| {
            states
                .iter()
                .map(|state| text(&state.node, "function"))
                .collect()
        })
    })
}

#[test]
fn controllers_of_a_state_under_two_controllers_are_named_once_each() {
    assert_eq!(
        controllers_of("/soc@0/mixed@1000"),
        Some(vec![
            "test,pinctrl".to_owned(),
            "qcom,pm660l-gpio".to_owned()
        ])
    );
}

#[test]
fn a_node_without_a_state_has_no_controllers() {
    assert_eq!(controllers_of("/soc@0/plain@3000"), Some(vec![]));
    assert_eq!(controllers_of("/soc@0/absent@5000"), Some(vec![]));
}

#[test]
fn the_default_state_is_the_entry_named_default() {
    assert_eq!(
        functions_of("/soc@0/spi@2000"),
        Some(vec!["spi".to_owned()])
    );
}

#[test]
fn a_reference_without_a_node_is_unresolved() {
    assert_eq!(functions_of("/soc@0/dangling@4000"), None);
}
