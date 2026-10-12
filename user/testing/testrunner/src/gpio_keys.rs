//! Разбор узла `gpio-keys` и окно антидребезга `driver-gpio-keys` на синтетических входах и
//! отказ подключения узла без состояния `default`.

use alloc::{string::String, vec::Vec};

use component_abi::RequirementDeclaration;
use driver_gpio_keys::{Edge, Key, Keys, KeysError, Window, keys};
use fdt_test::Token;
use input_abi::{Usage, input};
use ipc::Protocol;
use platform_abi::{Description, DeviceId};

use crate::{
    composition::Import,
    log_relay::await_component_lines_with_prefix,
    rootkeeper_system::{finish, with_description, with_drivers},
};

const TREE: &[Token<'static>] = &fdt_test::tree! {
    side-keys {
        compatible = "gpio-keys";
        label = "side-keys";
        down {
            gpios = <1 2 1>;
            "linux,code" = <114>;
            "debounce-interval" = <15>;
        };
        off {
            status = "disabled";
            gpios = <1 3 1>;
            "linux,code" = <116>;
        };
        up {
            gpios = <1 4 1>;
            "linux,code" = <115>;
        };
        down-too {
            gpios = <1 10 1>;
            "linux,code" = <114>;
        };
    };
    unnamed {
        compatible = "gpio-keys";
        key {
            gpios = <1 5 1>;
            "linux,code" = <116>;
        };
    };
    uncoded {
        compatible = "gpio-keys";
        key {
            gpios = <1 6 1>;
        };
    };
    untranslated {
        compatible = "gpio-keys";
        key {
            gpios = <1 7 1>;
            "linux,code" = <0x2ff>;
        };
    };
    switch {
        compatible = "gpio-keys";
        key {
            gpios = <1 8 1>;
            "linux,code" = <116>;
            "linux,input-type" = <5>;
        };
    };
    lineless {
        compatible = "gpio-keys";
        key {
            "linux,code" = <116>;
        };
    };
    hidden {
        compatible = "gpio-keys";
        key {
            status = "disabled";
            gpios = <1 9 1>;
            "linux,code" = <116>;
        };
    };
};

const PROGRAM: &str = "driver-gpio-keys";

const INPUT: &str = "input";

const IMPORTS: &[Import] = &[Import {
    declaration: RequirementDeclaration::session(INPUT, input::Protocol::ID),
    namespace_root: None,
}];

const MS: u64 = 1_000_000;

fn parsed<'d>(description: &'d Description<'_>, node: &str) -> Result<Keys<'d>, KeysError> {
    keys(description, &DeviceId::new(node).expect("absolute path"))
}

fn key(node: &str, usage: usize, debounce: u64) -> Key {
    Key {
        node: DeviceId::new(node).expect("absolute path"),
        usage,
        debounce,
    }
}

#[test_harness::test]
fn the_available_children_give_the_keys_of_the_device_named_by_its_label_and_share_a_code() {
    let blob = fdt_test::build(TREE);
    let description = Description::new(&blob, 0).expect("tree parses");

    let side = parsed(&description, "/side-keys");
    let unnamed = parsed(&description, "/unnamed").map(|keys| keys.name);

    test_harness::check_eq!(
        side,
        Ok(Keys {
            name: "side-keys",
            usages: alloc::vec![Usage::VOLUME_DOWN, Usage::VOLUME_UP],
            keys: alloc::vec![
                key("/side-keys/down", 0, 15 * MS),
                key("/side-keys/up", 1, 5 * MS),
                key("/side-keys/down-too", 0, 5 * MS),
            ],
        })
    );
    test_harness::check_eq!(unnamed, Ok("gpio-keys"));
}

#[test_harness::test]
fn a_child_without_a_line_or_a_key_code_refuses_the_node() {
    let blob = fdt_test::build(TREE);
    let description = Description::new(&blob, 0).expect("tree parses");

    let refused = [
        "/uncoded",
        "/untranslated",
        "/switch",
        "/lineless",
        "/hidden",
    ]
    .map(|node| parsed(&description, node).map(drop));

    test_harness::check_eq!(
        refused,
        [
            Err(KeysError::NoCode),
            Err(KeysError::UnknownCode(0x2ff)),
            Err(KeysError::InputType(5)),
            Err(KeysError::NoLine),
            Err(KeysError::NoKeys),
        ]
    );
}

#[test_harness::test]
fn a_bouncing_press_is_reported_once_at_the_end_of_the_last_window() {
    let mut window = Window::new(15 * MS);

    for now in [0, 4 * MS, 9 * MS] {
        window.interrupt(now);
    }
    let deadline = window.deadline();
    let settled = window.settle(true);

    test_harness::check_eq!(deadline, Some(24 * MS));
    test_harness::check_eq!(settled, Some(Edge::Press));
    test_harness::check_eq!(window.deadline(), None);
}

#[test_harness::test]
fn an_unchanged_level_reports_nothing_and_a_changed_one_reports_its_edge() {
    let mut window = Window::new(5 * MS);
    let held_at_attach = Window::new(5 * MS).settle(true);

    let released = window.settle(false);
    let pressed = window.settle(true);
    let held = window.settle(true);
    let let_go = window.settle(false);

    test_harness::check_eq!(
        [released, pressed, held, let_go],
        [None, Some(Edge::Press), None, Some(Edge::Release)]
    );
    test_harness::check_eq!(held_at_attach, Some(Edge::Press));
}

#[test_harness::test]
fn a_node_without_a_default_state_is_refused_and_logged() {
    let described = with_description(|description| {
        description
            .compatible_devices("gpio-keys")
            .next()
            .and_then(|device| {
                let node = description.node(device.id())?;
                let stateless = node.bytes("pinctrl-0").is_none();
                Some((String::from(device.id().as_str()), stateless))
            })
    });
    let Some((node, true)) = described else {
        test_harness::skip!("gpio-keys without pinctrl-0 is not described");
    };

    let composition = with_drivers();
    let importer = composition.importer(IMPORTS);
    let input = importer.session::<input::Protocol>(INPUT);

    let refusal = alloc::format!("{node} not bound: ");
    let lines: Vec<String> = await_component_lines_with_prefix(PROGRAM, &refusal, 1)
        .into_iter()
        .filter(|line| line.starts_with(&refusal))
        .collect();

    test_harness::check_eq!(lines, [alloc::format!("{refusal}Controllers(0)")]);
    drop(input);
    finish(composition);
}
