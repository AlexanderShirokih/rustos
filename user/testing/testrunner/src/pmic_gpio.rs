//! Применение состояния `default` и линии контроллера GPIO PMIC `pinctrl-spmi-gpio` на модели
//! регистров и клиент `Pins` контроллера платы.

use alloc::vec::Vec;
use core::cell::Cell;

use component_abi::SessionError;
use fdt_test::Token;
use ipc::CallError;
use kernel_objects::{Session, Signal};
use pinctrl_spmi_gpio::{Gpio, Registers, line, plan};
use pins_abi::{Claims, PinsError, device_pins, pins};
use platform_abi::{Description, DeviceId};
use runtime::Timeout;
use spmi_abi::DeviceError;
use syscall::SIGNALED;

use crate::{
    composition::{controller_provider, pause, request},
    pins::{PINS_IMPORTS, PINS_REPLY_TIMEOUT, PINS_REQUIREMENT, controller_compatibles},
    register_model::{Access, RegisterFile},
    rootkeeper_system::{
        POLL_ATTEMPTS, POLL_INTERVAL, await_revoked, finish, platform_describes, with_description,
        with_drivers,
    },
};

const TREE: &[Token<'static>] = &fdt_test::tree! {
    soc@0 {
        pinctrl@3100000 {
            compatible = "test,pinctrl";
            #gpio-cells = <2>;
            phandle = <40>;
            tlmm-default {
                pins = "gpio1";
                function = "gpio";
                phandle = <41>;
            };
        };
        spmi@800f000 {
            compatible = "qcom,spmi-pmic-arb";
            #address-cells = <2>;
            #size-cells = <0>;
            pmic@2 {
                compatible = "qcom,pm660l", "qcom,spmi-pmic";
                reg = <2 0>;
                #address-cells = <1>;
                #size-cells = <0>;
                gpio@c000 {
                    compatible = "qcom,pm660l-gpio", "qcom,spmi-gpio";
                    reg = <0xc000>;
                    #gpio-cells = <2>;
                    phandle = <1>;
                    key-default {
                        pins = "gpio7";
                        function = "normal";
                        bias-pull-up;
                        input-enable;
                        "qcom,drive-strength" = <0>;
                        phandle = <2>;
                    };
                    beyond {
                        pins = "gpio13";
                        function = "normal";
                        phandle = <3>;
                    };
                    func1 {
                        pins = "gpio2";
                        function = "func1";
                        phandle = <4>;
                    };
                    strong {
                        pins = "gpio3";
                        "qcom,drive-strength" = <4>;
                        phandle = <5>;
                    };
                    pull-down {
                        pins = "gpio4";
                        bias-pull-down;
                        "qcom,drive-strength" = <1>;
                        phandle = <6>;
                    };
                    pull-disable {
                        pins = "gpio5";
                        bias-disable;
                        "qcom,drive-strength" = <2>;
                        phandle = <7>;
                    };
                    weak {
                        pins = "gpio6";
                        "qcom,drive-strength" = <3>;
                        phandle = <8>;
                    };
                    bare {
                        pins = "gpio8";
                        function = "normal";
                        phandle = <9>;
                    };
                    mixed {
                        pins = "gpio9";
                        function = "normal";
                        phandle = <10>;
                    };
                };
                gpio@d000 {
                    compatible = "qcom,pm8998-gpio", "qcom,spmi-gpio";
                    reg = <0xd000>;
                    #gpio-cells = <2>;
                    phandle = <20>;
                    other-default {
                        pins = "gpio1";
                        function = "normal";
                        phandle = <21>;
                    };
                };
            };
        };
    };
    key { "pinctrl-names" = "default"; "pinctrl-0" = <2>; };
    beyond { "pinctrl-names" = "default"; "pinctrl-0" = <3>; };
    func1 { "pinctrl-names" = "default"; "pinctrl-0" = <4>; };
    strong { "pinctrl-names" = "default"; "pinctrl-0" = <5>; };
    pull-down { "pinctrl-names" = "default"; "pinctrl-0" = <6>; };
    pull-disable { "pinctrl-names" = "default"; "pinctrl-0" = <7>; };
    weak { "pinctrl-names" = "default"; "pinctrl-0" = <8>; };
    bare { "pinctrl-names" = "default"; "pinctrl-0" = <9>; };
    foreign { "pinctrl-names" = "default"; "pinctrl-0" = <21>; };
    mixed { "pinctrl-names" = "default"; "pinctrl-0" = <41 10>; };
    keys {
        up { gpios = <1 7 1>; };
        down { gpios = <1 10 0>; };
        odd { gpios = <1 11 2>; };
        tlmm { gpios = <40 1 0>; };
    };
};

const BOARD_CONTROLLER: &str = "/soc@0/spmi@800f000/pmic@2/gpio@c000";
const BOARD_KEYS: &str = "/gpio-keys";
const BOARD_VOLUME_UP: &str = "/gpio-keys/key-volup";
const BOARD_OUTSIDE: &str = "/extcon-usb";

const BUS: usize = 0;

const TYPE: u16 = 0x04;
const SUBTYPE: u16 = 0x05;
const STATUS: u16 = 0x10;
const MODE: u16 = 0x40;
const PULL: u16 = 0x42;
const LV_MV_FUNCTION: u16 = 0x44;
const DRIVE: u16 = 0x45;
const ENABLE: u16 = 0x46;

const GPIO_TYPE: u32 = 0x10;
const SUBTYPE_GPIO_4CH: u32 = 0x01;
const SUBTYPE_LV: u32 = 0x10;

const ENABLED: u32 = 1 << 7;

struct Model {
    registers: RegisterFile,
    refusal: Cell<Option<DeviceError>>,
}

impl Model {
    fn new() -> Self {
        Self {
            registers: RegisterFile::new(),
            refusal: Cell::new(None),
        }
    }

    fn pin(&self, block: u8, subtype: u32) {
        self.set(block, TYPE, GPIO_TYPE);
        self.set(block, SUBTYPE, subtype);
    }

    fn get(&self, block: u8, register: u16) -> u32 {
        self.registers.get(BUS, address(block, register))
    }

    fn set(&self, block: u8, register: u16, value: u32) {
        self.registers.set(BUS, address(block, register), value);
    }

    fn stores(&self) -> Vec<usize> {
        self.registers
            .take_accesses()
            .into_iter()
            .filter_map(|access| match access {
                Access::Store { offset, .. } => Some(offset),
                Access::Load { .. } => None,
            })
            .collect()
    }
}

impl Registers for &Model {
    fn read(&self, address: u16) -> Result<u8, DeviceError> {
        let value = self.registers.load(BUS, usize::from(address));
        Ok(u8::try_from(value).expect("byte register"))
    }

    fn update(&self, address: u16, mask: u8, value: u8) -> Result<(), DeviceError> {
        if let Some(refusal) = self.refusal.get() {
            return Err(refusal);
        }
        let current = self.registers.load(BUS, usize::from(address));
        let updated = current & !u32::from(mask) | u32::from(value & mask);
        self.registers.store(BUS, usize::from(address), updated);
        Ok(())
    }
}

const fn address(block: u8, register: u16) -> usize {
    (block as usize) << 8 | register as usize
}

fn open(blob: &[u8], model: &Model, device: &str) -> Result<(), PinsError> {
    let description = Description::new(blob, 0).expect("tree parses");
    let device = DeviceId::new(device).expect("absolute path");
    let plan = plan(&description, &device, &Claims::new())?;
    let mut gpio = Gpio::new(model);
    for controller in plan.controllers() {
        gpio.apply(&plan.settings(&controller.id))?;
    }
    Ok(())
}

#[test_harness::test]
fn the_volume_key_state_lands_in_block_0xc6_with_pull_up_input_and_enable() {
    let blob = fdt_test::build(TREE);
    let model = Model::new();
    model.pin(0xC6, SUBTYPE_GPIO_4CH);
    model.set(0xC6, MODE, 0x70);
    model.set(0xC6, PULL, 0x05);
    model.set(0xC6, DRIVE, 0x03);

    let opened = open(&blob, &model, "/key");
    let stores = model.stores();

    test_harness::check_eq!(opened, Ok(()));
    test_harness::check!(stores.iter().all(|offset| offset >> 8 == 0xC6));
    test_harness::check_eq!(model.get(0xC6, MODE), 0);
    test_harness::check_eq!(model.get(0xC6, PULL), 0);
    test_harness::check_eq!(model.get(0xC6, DRIVE), 0);
    test_harness::check_eq!(model.get(0xC6, ENABLE), ENABLED);
}

#[test_harness::test]
fn a_set_keeps_the_fields_it_does_not_name() {
    let blob = fdt_test::build(TREE);
    let model = Model::new();
    model.pin(0xC7, SUBTYPE_GPIO_4CH);
    model.set(0xC7, MODE, 0x10);
    model.set(0xC7, PULL, 0x05);
    model.set(0xC7, DRIVE, 0x02);

    let opened = open(&blob, &model, "/bare");

    test_harness::check_eq!(opened, Ok(()));
    test_harness::check_eq!(model.get(0xC7, MODE), 0x10);
    test_harness::check_eq!(model.get(0xC7, PULL), 0x05);
    test_harness::check_eq!(model.get(0xC7, DRIVE), 0x02);
    test_harness::check_eq!(model.get(0xC7, ENABLE), ENABLED);
}

#[test_harness::test]
fn mode_and_function_follow_the_layout_of_the_subtype() {
    let blob = fdt_test::build(TREE);
    let lv = Model::new();
    lv.pin(0xC6, SUBTYPE_LV);
    lv.set(0xC6, MODE, 0xFF);
    lv.set(0xC6, LV_MV_FUNCTION, 0xFF);
    let shared = Model::new();
    shared.pin(0xC6, SUBTYPE_GPIO_4CH);
    shared.set(0xC6, MODE, 0xFF);

    let opened = [open(&blob, &lv, "/key"), open(&blob, &shared, "/key")];

    test_harness::check_eq!(opened, [Ok(()), Ok(())]);
    test_harness::check_eq!(lv.get(0xC6, MODE), 0xFC);
    test_harness::check_eq!(lv.get(0xC6, LV_MV_FUNCTION), 0xF0);
    test_harness::check_eq!(shared.get(0xC6, MODE), 0x81);
}

#[test_harness::test]
fn pulls_and_drive_strengths_take_their_fields() {
    let blob = fdt_test::build(TREE);
    let model = Model::new();
    for block in [0xC3, 0xC4, 0xC5] {
        model.pin(block, SUBTYPE_GPIO_4CH);
    }

    let opened = ["/pull-down", "/pull-disable", "/weak"].map(|node| open(&blob, &model, node));

    test_harness::check_eq!(opened, [Ok(()), Ok(()), Ok(())]);
    test_harness::check_eq!([0xC3, 0xC4].map(|block| model.get(block, PULL)), [4, 5]);
    test_harness::check_eq!(
        [0xC3, 0xC4, 0xC5].map(|block| model.get(block, DRIVE)),
        [3, 2, 1]
    );
}

#[test_harness::test]
fn a_refused_state_writes_no_register() {
    let blob = fdt_test::build(TREE);
    let model = Model::new();
    for block in 0xC0..=0xCB {
        model.pin(block, SUBTYPE_GPIO_4CH);
    }
    model.set(0xC6, TYPE, 0x11);

    let planned = ["/beyond", "/func1", "/strong", "/foreign"].map(|node| {
        let opened = open(&blob, &model, node);
        (opened, model.registers.take_accesses())
    });
    let mistyped = open(&blob, &model, "/key");
    let mistyped_stores = model.stores();

    test_harness::check_eq!(
        planned,
        [const { (Err(PinsError::Unsupported), Vec::new()) }; 4]
    );
    test_harness::check_eq!(mistyped, Err(PinsError::Unsupported));
    test_harness::check_eq!(mistyped_stores, []);
}

#[test_harness::test]
fn a_set_of_another_controller_is_skipped() {
    let blob = fdt_test::build(TREE);
    let model = Model::new();
    model.pin(0xC8, SUBTYPE_GPIO_4CH);

    let opened = open(&blob, &model, "/mixed");
    let stores = model.stores();

    test_harness::check_eq!(opened, Ok(()));
    test_harness::check!(!stores.is_empty());
    test_harness::check!(stores.iter().all(|offset| offset >> 8 == 0xC8));
}

fn record(blob: &[u8], node: &str) -> Result<(), PinsError> {
    let description = Description::new(blob, 0).expect("tree parses");
    let session = DeviceId::new("/keys").expect("absolute path");
    let node = DeviceId::new(node).expect("absolute path");
    line(&description, &session, &node, "gpios", 0).map(drop)
}

#[test_harness::test]
fn a_line_outside_the_session_or_the_controller_is_refused() {
    let blob = fdt_test::build(TREE);

    test_harness::check_eq!(record(&blob, "/keys/odd"), Err(PinsError::Unsupported));
    test_harness::check_eq!(record(&blob, "/keys/tlmm"), Err(PinsError::Unsupported));
    test_harness::check_eq!(record(&blob, "/key"), Err(PinsError::NoDevice));
}

#[test_harness::test]
fn a_line_becomes_an_input_and_reads_its_level_through_the_polarity() {
    let blob = fdt_test::build(TREE);
    let description = Description::new(&blob, 0).expect("tree parses");
    let session = DeviceId::new("/keys").expect("absolute path");
    let up = DeviceId::new("/keys/up").expect("absolute path");
    let down = DeviceId::new("/keys/down").expect("absolute path");
    let model = Model::new();
    model.pin(0xC6, SUBTYPE_GPIO_4CH);
    model.pin(0xC9, SUBTYPE_GPIO_4CH);
    model.set(0xC6, MODE, 0x10);
    let mut gpio = Gpio::new(&model);

    let low = line(&description, &session, &up, "gpios", 0).expect("active-low line");
    let high = line(&description, &session, &down, "gpios", 0).expect("active-high line");
    let blocks = [low.block, high.block];
    let input = gpio.input(low.block);
    let mode = model.get(0xC6, MODE);
    let enable = model.get(0xC6, ENABLE);

    model.set(0xC6, STATUS, 0);
    model.set(0xC9, STATUS, 0);
    let released = [
        low.active(gpio.level(0xC6) == Ok(true)),
        high.active(gpio.level(0xC9) == Ok(true)),
    ];
    model.set(0xC6, STATUS, 1);
    model.set(0xC9, STATUS, 1);
    let raised = [
        low.active(gpio.level(0xC6) == Ok(true)),
        high.active(gpio.level(0xC9) == Ok(true)),
    ];

    test_harness::check_eq!(blocks, [0xC6, 0xC9]);
    test_harness::check_eq!(input, Ok(()));
    test_harness::check_eq!(mode, 0);
    test_harness::check_eq!(enable, ENABLED);
    test_harness::check_eq!(released, [true, false]);
    test_harness::check_eq!(raised, [false, true]);
}

#[test_harness::test]
fn a_refused_bus_write_is_denied_or_failed() {
    let model = Model::new();
    model.pin(0xC6, SUBTYPE_GPIO_4CH);
    let mut gpio = Gpio::new(&model);

    let outcomes = [DeviceError::Denied, DeviceError::Failed].map(|refusal| {
        model.refusal.set(Some(refusal));
        gpio.input(0xC6)
    });

    test_harness::check_eq!(outcomes, [Err(PinsError::Denied), Err(PinsError::Failed)]);
}

#[test_harness::test]
fn the_board_volume_up_line_reads_released_and_its_signal_ends_with_the_session() {
    if !platform_describes(BOARD_CONTROLLER) {
        test_harness::skip!("{} is not described", BOARD_CONTROLLER);
    }

    let controller =
        with_description(|description| controller_compatibles(description, BOARD_KEYS));

    let composition = with_drivers();
    let importer = composition.importer(PINS_IMPORTS);
    let unfiltered =
        request::<pins::Protocol>(importer.handle(), PINS_REQUIREMENT, PINS_REPLY_TIMEOUT)
            .map(drop);
    let client =
        controller_provider::<pins::Protocol>(importer.handle(), PINS_REQUIREMENT, &controller)
            .client()
            .with_wait(PINS_REPLY_TIMEOUT);

    let keys = client
        .open(DeviceId::new(BOARD_KEYS).expect("absolute path"))
        .map(|session| {
            Session::<device_pins::Protocol>::from_session(session)
                .client()
                .with_wait(PINS_REPLY_TIMEOUT)
        })
        .expect("gpio-keys session");
    let line = keys.input(
        DeviceId::new(BOARD_VOLUME_UP).expect("absolute path"),
        "gpios",
        0,
    );
    let active = line.as_ref().map(|line| keys.active(*line));
    let outside = keys.input(
        DeviceId::new(BOARD_OUTSIDE).expect("absolute path"),
        "id-gpios",
        0,
    );
    let signal = line
        .as_ref()
        .ok()
        .and_then(|line| keys.irq(*line).ok())
        .map(Signal::from);

    drop(keys);

    let revoked = signal
        .as_ref()
        .is_some_and(|signal| await_revoked(|| signal.wait(SIGNALED, Timeout::from_millis(0))));
    let reopened = (0..POLL_ATTEMPTS).any(|_| {
        match client.open(DeviceId::new(BOARD_KEYS).expect("absolute path")) {
            Err(CallError::Remote(PinsError::Busy)) => {
                pause(POLL_INTERVAL);
                false
            }
            opened => opened.is_ok(),
        }
    });

    test_harness::check_eq!(unfiltered, Err(CallError::Remote(SessionError::Unbound)));
    test_harness::check!(line.is_ok());
    test_harness::check_eq!(active, Ok(Ok(false)));
    test_harness::check_eq!(outside, Err(CallError::Remote(PinsError::NoDevice)));
    test_harness::check!(signal.is_some());
    test_harness::check!(revoked);
    test_harness::check!(reopened);

    finish(composition);
}
