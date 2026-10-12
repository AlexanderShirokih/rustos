//! Применение состояния `default` контроллером TLMM `pinctrl-msm` на модели регистров и
//! клиент `Pins` контроллера платы.

use alloc::{string::String, vec::Vec};

use component_abi::{RequirementDeclaration, SessionError};
use fdt_test::Token;
use ipc::{CallError, Protocol};
use pinctrl_msm::{PinGroup, Registers, Tile, Tlmm};
use pins_abi::{Claims, PinsError, pins};
use platform_abi::{Description, DeviceId};
use runtime::Timeout;

use crate::{
    composition::{Import, compatibles, controller_provider, request},
    register_model::{Access, RegisterFile},
    rootkeeper_system::{finish, platform_describes, with_description, with_drivers},
};

const CONTROLLER: &str = "/soc@0/pinctrl@1000000";

const NORTH_WINDOW: usize = 0;
const SOUTH_WINDOW: usize = 1;
const WINDOW_SIZE: usize = 0x40_0000;

const PIN_STRIDE: usize = 0x1000;
const IO: usize = 0x4;

const FUNCTION_SHIFT: u32 = 2;
const DRIVE_SHIFT: u32 = 6;
const OE: u32 = 1 << 9;
const IO_IN: u32 = 1 << 0;
const IO_OUT: u32 = 1 << 1;

const I2C_HOST: &str = "/soc@0/i2c@a000";
const SPI_HOST: &str = "/soc@0/spi@b000";
const SHARED_UART_HOST: &str = "/soc@0/serial@d000";
const SDC_HOST: &str = "/soc@0/mmc@f000";
const FOREIGN_FUNCTION_HOST: &str = "/soc@0/i2c@10000";
const WEAK_DRIVE_HOST: &str = "/soc@0/i2c@11000";
const UNKNOWN_PROPERTY_HOST: &str = "/soc@0/i2c@12000";
const DANGLING_HOST: &str = "/soc@0/i2c@13000";
const MIXED_PROVIDERS_HOST: &str = "/soc@0/i2c@14000";
const PROTECTED_SDC_HOST: &str = "/soc@0/mmc@16000";
const UNNAMED_STATE_HOST: &str = "/soc@0/i2c@17000";
const GPIO_HOST: &str = "/soc@0/led@18000";
const CONTAINER_HOST: &str = "/soc@0/spi@19000";
const CONTAINER_PROPERTY_HOST: &str = "/soc@0/spi@1a000";

const GROUPS: &[PinGroup] = &[
    group(Tile::South, ["", "uart", "", "", "", "", "", "", ""]),
    group(Tile::South, ["", "uart", "", "", "", "", "", "", ""]),
    group(Tile::South, ["", "uart", "i2c", "", "", "", "", "", ""]),
    group(Tile::South, ["", "uart", "i2c", "", "", "", "", "", ""]),
    group(Tile::North, ["spi", "", "", "", "", "", "", "", ""]),
    group(Tile::South, [""; 9]),
];

const TREE: &[Token<'static>] = &fdt_test::tree! {
    soc@0 {
        pinctrl@1000000 {
            compatible = "test,pinctrl";
            "reg-names" = "north", "south", "center";
            "gpio-reserved-ranges" = <8 4>;
            i2c1-default {
                pins = "gpio2", "gpio3";
                function = "i2c";
                drive-strength = <2>;
                bias-pull-up;
                phandle = <1>;
            };
            spi2-default {
                pins = "gpio4";
                function = "spi";
                phandle = <2>;
            };
            uart1-wide {
                pins = "gpio0", "gpio1", "gpio2", "gpio3";
                function = "uart";
                phandle = <4>;
            };
            sdc1-default {
                pins = "sdc1_clk";
                bias-disable;
                phandle = <6>;
            };
            foreign-function {
                pins = "gpio4";
                function = "i2c";
                phandle = <7>;
            };
            weak-drive {
                pins = "gpio2";
                function = "i2c";
                drive-strength = <3>;
                phandle = <8>;
            };
            unknown-property {
                pins = "gpio2";
                function = "i2c";
                output-high;
                phandle = <9>;
            };
            protected-sdc {
                pins = "gpio9", "sdc1_clk";
                phandle = <11>;
            };
            led-gpio {
                pins = "gpio5";
                function = "gpio";
                phandle = <12>;
            };
            spi3-default {
                phandle = <13>;
                clk {
                    pins = "gpio4";
                    function = "spi";
                };
                cs {
                    pins = "gpio5";
                    function = "gpio";
                };
            };
            spi4-default {
                function = "spi";
                phandle = <14>;
                clk {
                    pins = "gpio4";
                };
            };
        };
        gpio@c000 {
            compatible = "qcom,pm660-gpio";
            foreign-pins {
                pins = "gpio1";
                function = "normal";
                phandle = <20>;
            };
        };
        i2c@a000 {
            "pinctrl-names" = "default";
            "pinctrl-0" = <1>;
        };
        spi@b000 {
            "pinctrl-names" = "default";
            "pinctrl-0" = <2>;
        };
        serial@d000 {
            "pinctrl-names" = "default";
            "pinctrl-0" = <4>;
        };
        mmc@f000 {
            "pinctrl-names" = "default";
            "pinctrl-0" = <6>;
        };
        i2c@10000 {
            "pinctrl-names" = "default";
            "pinctrl-0" = <7>;
        };
        i2c@11000 {
            "pinctrl-names" = "default";
            "pinctrl-0" = <8>;
        };
        i2c@12000 {
            "pinctrl-names" = "default";
            "pinctrl-0" = <9>;
        };
        i2c@13000 {
            "pinctrl-names" = "default";
            "pinctrl-0" = <1 99>;
        };
        i2c@14000 {
            "pinctrl-names" = "default";
            "pinctrl-0" = <1 20>;
        };
        mmc@16000 {
            "pinctrl-names" = "default";
            "pinctrl-0" = <11>;
        };
        i2c@17000 {
            "pinctrl-0" = <1>;
        };
        led@18000 {
            "pinctrl-names" = "default";
            "pinctrl-0" = <12>;
        };
        spi@19000 {
            "pinctrl-names" = "default";
            "pinctrl-0" = <13>;
        };
        spi@1a000 {
            "pinctrl-names" = "default";
            "pinctrl-0" = <14>;
        };
    };
};

const BOARD_CONTROLLER: &str = "/soc@0/pinctrl@3100000";
const BOARD_SDC_HOST: &str = "/soc@0/mmc@c084000";
const BOARD_NO_STATE: &str = "/extcon-usb";

pub(crate) const PINS_REQUIREMENT: &str = "pins";
pub(crate) const PINS_REPLY_TIMEOUT: Timeout = Timeout::from_secs(5);

pub(crate) const PINS_IMPORTS: &[Import] = &[Import {
    declaration: RequirementDeclaration::all(PINS_REQUIREMENT, pins::Protocol::ID),
    namespace_root: None,
}];

struct Windows<'a> {
    registers: &'a RegisterFile,
    north: usize,
    center: usize,
    south: usize,
}

struct Controller<'a> {
    description: Description<'a>,
    tlmm: Tlmm<Windows<'a>>,
    claims: Claims,
}

impl Registers for Windows<'_> {
    fn size(&self, _tile: Tile) -> usize {
        WINDOW_SIZE
    }

    fn load(&self, tile: Tile, offset: usize) -> u32 {
        self.registers.load(self.window(tile), offset)
    }

    fn store(&self, tile: Tile, offset: usize, value: u32) {
        self.registers.store(self.window(tile), offset, value);
    }
}

impl Windows<'_> {
    fn window(&self, tile: Tile) -> usize {
        match tile {
            Tile::North => self.north,
            Tile::Center => self.center,
            Tile::South => self.south,
        }
    }
}

impl<'a> Controller<'a> {
    fn new(blob: &'a [u8], registers: &'a RegisterFile) -> Self {
        let description = Description::new(blob, 0).expect("tree parses");
        let id = DeviceId::new(CONTROLLER).expect("absolute path");
        let node = description.node(&id).expect("controller node");
        let window = |tile: Tile| usize::from(tile.resource(&node).expect("named window"));
        let windows = Windows {
            registers,
            north: window(Tile::North),
            center: window(Tile::Center),
            south: window(Tile::South),
        };
        let tlmm = Tlmm::new(windows, GROUPS, &description, id).expect("model controller");

        Self {
            description,
            tlmm,
            claims: Claims::new(),
        }
    }

    fn open(&mut self, badge: u64, device: &str) -> Result<(), PinsError> {
        let device = DeviceId::new(device).expect("absolute path");
        let plan = self.tlmm.plan(&self.description, &device, &self.claims)?;
        self.tlmm.apply(&plan);
        plan.claim(&mut self.claims, badge);
        Ok(())
    }

    fn release(&mut self, badge: u64) {
        self.claims.release(badge);
    }
}

const fn group(tile: Tile, functions: [&'static str; 9]) -> PinGroup {
    PinGroup { tile, functions }
}

const fn ctl(pin: usize) -> usize {
    pin * PIN_STRIDE
}

const fn io(pin: usize) -> usize {
    pin * PIN_STRIDE + IO
}

fn stores(accesses: &[Access]) -> Vec<(usize, usize)> {
    accesses
        .iter()
        .filter_map(|access| match *access {
            Access::Store { window, offset, .. } => Some((window, offset)),
            Access::Load { .. } => None,
        })
        .collect()
}

#[test_harness::test]
fn a_north_pin_set_without_drive_strength_keeps_its_drive_in_the_north_window() {
    let blob = fdt_test::build(TREE);
    let registers = RegisterFile::new();
    registers.set(NORTH_WINDOW, ctl(4), 4 << DRIVE_SHIFT);
    let mut controller = Controller::new(&blob, &registers);

    let opened = controller.open(1, SPI_HOST);

    test_harness::check_eq!(opened, Ok(()));
    test_harness::check_eq!(
        registers.take_accesses(),
        [
            Access::Load {
                window: NORTH_WINDOW,
                offset: ctl(4),
            },
            Access::Store {
                window: NORTH_WINDOW,
                offset: ctl(4),
                value: 4 << DRIVE_SHIFT | 1 << FUNCTION_SHIFT,
            },
        ]
    );
}

#[test_harness::test]
fn i2c_pins_take_their_function_pull_and_drive_and_skip_a_set_of_another_provider() {
    let blob = fdt_test::build(TREE);
    let registers = RegisterFile::new();
    let mut controller = Controller::new(&blob, &registers);

    let opened = controller.open(1, MIXED_PROVIDERS_HOST);
    let expected = 3 << FUNCTION_SHIFT | 3;

    test_harness::check_eq!(opened, Ok(()));
    test_harness::check_eq!(
        stores(&registers.take_accesses()),
        [(SOUTH_WINDOW, ctl(2)), (SOUTH_WINDOW, ctl(3))]
    );
    test_harness::check_eq!(registers.get(SOUTH_WINDOW, ctl(2)), expected);
    test_harness::check_eq!(registers.get(SOUTH_WINDOW, ctl(3)), expected);
}

#[test_harness::test]
fn a_state_without_pins_applies_its_child_sets() {
    let blob = fdt_test::build(TREE);
    let registers = RegisterFile::new();
    let mut controller = Controller::new(&blob, &registers);

    let opened = controller.open(1, CONTAINER_HOST);

    test_harness::check_eq!(opened, Ok(()));
    test_harness::check_eq!(
        stores(&registers.take_accesses()),
        [(NORTH_WINDOW, ctl(4)), (SOUTH_WINDOW, ctl(5))]
    );
}

#[test_harness::test]
fn a_refused_state_leaves_registers_untouched() {
    let blob = fdt_test::build(TREE);
    let registers = RegisterFile::new();
    let mut controller = Controller::new(&blob, &registers);
    let refusals = [
        (SDC_HOST, PinsError::Unsupported),
        (FOREIGN_FUNCTION_HOST, PinsError::Unsupported),
        (WEAK_DRIVE_HOST, PinsError::Unsupported),
        (UNKNOWN_PROPERTY_HOST, PinsError::Unsupported),
        (CONTAINER_PROPERTY_HOST, PinsError::Unsupported),
        (DANGLING_HOST, PinsError::Unsupported),
        (PROTECTED_SDC_HOST, PinsError::Denied),
        (UNNAMED_STATE_HOST, PinsError::NoDevice),
    ];

    let outcomes: Vec<_> = refusals
        .iter()
        .map(|(host, _)| {
            let opened = controller.open(1, host);
            (*host, opened, registers.take_accesses())
        })
        .collect();

    let expected: Vec<_> = refusals
        .iter()
        .map(|(host, refusal)| (*host, Err(*refusal), Vec::new()))
        .collect();
    test_harness::check_eq!(outcomes, expected);
}

#[test_harness::test]
fn a_held_node_and_a_held_pin_are_busy_until_release() {
    let blob = fdt_test::build(TREE);
    let registers = RegisterFile::new();
    let mut controller = Controller::new(&blob, &registers);

    let first = controller.open(1, SHARED_UART_HOST);
    registers.take_accesses();
    let same_node = controller.open(2, SHARED_UART_HOST);
    let shared_pins = controller.open(2, I2C_HOST);

    test_harness::check_eq!(first, Ok(()));
    test_harness::check_eq!(same_node, Err(PinsError::Busy));
    test_harness::check_eq!(shared_pins, Err(PinsError::Busy));
    test_harness::check_eq!(registers.take_accesses(), []);
    test_harness::check_eq!(registers.get(SOUTH_WINDOW, ctl(2)), 2 << FUNCTION_SHIFT);
    test_harness::check_eq!(registers.get(SOUTH_WINDOW, ctl(3)), 2 << FUNCTION_SHIFT);

    controller.release(1);
    test_harness::check_eq!(controller.open(2, I2C_HOST), Ok(()));
    test_harness::check_eq!(registers.get(SOUTH_WINDOW, ctl(2)), 3 << FUNCTION_SHIFT | 3);
}

#[test_harness::test]
fn a_driven_pin_holds_its_input_level_before_it_becomes_gpio() {
    let blob = fdt_test::build(TREE);
    let registers = RegisterFile::new();
    registers.set(SOUTH_WINDOW, ctl(5), OE | 2 << FUNCTION_SHIFT);
    registers.set(SOUTH_WINDOW, io(5), IO_IN);
    let mut controller = Controller::new(&blob, &registers);

    let opened = controller.open(1, GPIO_HOST);

    test_harness::check_eq!(opened, Ok(()));
    test_harness::check_eq!(
        registers.take_accesses(),
        [
            Access::Load {
                window: SOUTH_WINDOW,
                offset: ctl(5),
            },
            Access::Load {
                window: SOUTH_WINDOW,
                offset: io(5),
            },
            Access::Store {
                window: SOUTH_WINDOW,
                offset: io(5),
                value: IO_IN | IO_OUT,
            },
            Access::Store {
                window: SOUTH_WINDOW,
                offset: ctl(5),
                value: OE,
            },
        ]
    );
}

pub(crate) fn controller_compatibles(description: &Description<'_>, path: &str) -> Vec<String> {
    compatibles(pins_abi::controllers(description, path).unwrap_or_default())
}

#[test_harness::test]
fn the_board_controller_refuses_sdc_groups_and_nodes_without_state() {
    if !platform_describes(BOARD_CONTROLLER) {
        test_harness::skip!("{} is not described", BOARD_CONTROLLER);
    }

    let controller =
        with_description(|description| controller_compatibles(description, BOARD_SDC_HOST));

    let composition = with_drivers();
    let importer = composition.importer(PINS_IMPORTS);
    let unfiltered =
        request::<pins::Protocol>(importer.handle(), PINS_REQUIREMENT, PINS_REPLY_TIMEOUT)
            .map(drop);

    let client =
        controller_provider::<pins::Protocol>(importer.handle(), PINS_REQUIREMENT, &controller)
            .client()
            .with_wait(PINS_REPLY_TIMEOUT);

    let sdc = client
        .open(DeviceId::new(BOARD_SDC_HOST).expect("absolute path"))
        .map(drop);
    let no_state = client
        .open(DeviceId::new(BOARD_NO_STATE).expect("absolute path"))
        .map(drop);

    test_harness::check_eq!(unfiltered, Err(CallError::Remote(SessionError::Unbound)));
    test_harness::check_eq!(sdc, Err(CallError::Remote(PinsError::Unsupported)));
    test_harness::check_eq!(no_state, Err(CallError::Remote(PinsError::NoDevice)));

    finish(composition);
}
