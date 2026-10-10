#![allow(clippy::unwrap_used)]

use fdt_test::Token;
use platform_abi::{Description, DeviceId};
use spmi_abi::controller;

const ARBITER: &str = "qcom,spmi-pmic-arb";

const TREE: &[Token<'static>] = &fdt_test::tree! {
    soc@0 {
        #address-cells = <1>;
        #size-cells = <1>;
        spmi@800f000 {
            compatible = "qcom,spmi-pmic-arb";
            #address-cells = <2>;
            #size-cells = <0>;
            pmic@0 {
                compatible = "qcom,pm660", "qcom,spmi-pmic";
                reg = <0 0>;
                #address-cells = <1>;
                #size-cells = <0>;
                rtc@6000 {
                    compatible = "qcom,pm8941-rtc";
                    reg = <0x6000 0x6100>;
                };
                pon@800 {
                    compatible = "qcom,pm8998-pon";
                    reg = <0x800>;
                    pwrkey {
                        compatible = "qcom,pm8941-pwrkey";
                    };
                };
            };
        };
        serial@c170000 {
            compatible = "qcom,msm-uartdm-v1.4";
        };
    };
};

fn controller_of(path: &str) -> Option<String> {
    let blob = fdt_test::build(TREE);
    let description = Description::new(&blob, 0).unwrap();
    controller(&description, &DeviceId::new(path)?)
        .and_then(|node| node.string("compatible").unwrap())
        .map(str::to_owned)
}

#[test]
fn a_clock_below_its_pmic_belongs_to_the_arbiter() {
    assert_eq!(
        controller_of("/soc@0/spmi@800f000/pmic@0/rtc@6000").as_deref(),
        Some(ARBITER)
    );
}

#[test]
fn a_key_below_a_node_without_cells_belongs_to_the_arbiter() {
    assert_eq!(
        controller_of("/soc@0/spmi@800f000/pmic@0/pon@800/pwrkey").as_deref(),
        Some(ARBITER)
    );
}

#[test]
fn a_node_outside_the_bus_has_no_controller() {
    assert_eq!(controller_of("/soc@0/serial@c170000"), None);
    assert_eq!(controller_of("/soc@0/absent"), None);
}
