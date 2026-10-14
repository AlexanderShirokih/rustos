//! Применение состояния `default`, линии и их прерывания контроллера TLMM `pinctrl-msm` на
//! модели регистров и клиент `Pins` контроллера платы.

use alloc::{string::String, vec::Vec};

use component_abi::{RequirementDeclaration, SessionError};
use fdt_test::Token;
use ipc::{CallError, Protocol};
use mmio::Registers;
use pinctrl_msm::{Line, PinGroup, Tile, Tlmm};
use pins_abi::{Claims, PinsError, pins};
use platform_abi::Description;
use runtime::Timeout;

use crate::{
    composition::{Import, compatibles, controller_provider, device, request},
    register_model::{Access, RegisterFile},
    rootkeeper_system::{finish, platform_describes, with_description, with_drivers},
};

const CONTROLLER: &str = "/soc@0/pinctrl@1000000";

const NORTH_WINDOW: usize = 0;
const SOUTH_WINDOW: usize = 1;
const WINDOW_SIZE: usize = 0x40_0000;

const PIN_STRIDE: usize = 0x1000;
const IO: usize = 0x4;
const INTR_CFG: usize = 0x8;
const INTR_STATUS: usize = 0xc;

const PULL_UP: u32 = 3;
const FUNCTION_SHIFT: u32 = 2;
const DRIVE_SHIFT: u32 = 6;
const OE: u32 = 1 << 9;
const IO_IN: u32 = 1 << 0;
const IO_OUT: u32 = 1 << 1;

const INTR_ENABLE: u32 = 1 << 0;
const INTR_POLARITY: u32 = 1 << 1;
const DETECTION_SHIFT: u32 = 2;
const RAW_STATUS_EN: u32 = 1 << 4;
const TARGET_APPS: u32 = 3 << 5;
const INTR_LATCHED: u32 = 1 << 0;

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
const PANEL: &str = "/soc@0/panel@1b000";

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
            #gpio-cells = <2>;
            #interrupt-cells = <2>;
            phandle = <30>;
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
            #gpio-cells = <2>;
            #interrupt-cells = <2>;
            phandle = <31>;
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
        panel@1b000 {
            reset-gpios = <30 5 1>;
            enable-gpios = <30 2 0>;
            foreign-gpios = <31 1 0>;
            odd-gpios = <30 5 2>;
            protected-gpios = <30 9 0>;
            beyond-gpios = <30 40 0>;
            interrupts-extended = <30 5 1 30 5 2 30 5 3 30 5 4 30 5 8 30 5 0 30 9 1 30 40 1 31 1 1>;
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

impl Registers<Tile> for Windows<'_> {
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
        let id = device(CONTROLLER);
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

    fn open(&mut self, badge: u64, path: &str) -> Result<(), PinsError> {
        let device = device(path);
        let plan = self.tlmm.plan(&self.description, &device, &self.claims)?;
        self.tlmm.apply(&plan);
        plan.claim(&mut self.claims, badge);
        Ok(())
    }

    fn release(&mut self, badge: u64) {
        self.claims.release(badge);
    }

    fn line(&self, name: &str) -> Result<Line, PinsError> {
        let panel = device(PANEL);
        self.tlmm.line(&self.description, &panel, &panel, name, 0)
    }

    fn interrupt(&self, index: u8) -> Result<Line, PinsError> {
        let panel = device(PANEL);
        self.tlmm
            .interrupt(&self.description, &panel, &panel, index)
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

const fn intr_cfg(pin: usize) -> usize {
    pin * PIN_STRIDE + INTR_CFG
}

const fn intr_status(pin: usize) -> usize {
    pin * PIN_STRIDE + INTR_STATUS
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

fn armed(pin: usize, trigger: u32) -> Vec<Access> {
    let cfg = TARGET_APPS | RAW_STATUS_EN | trigger;
    Vec::from([
        Access::Load {
            window: SOUTH_WINDOW,
            offset: intr_cfg(pin),
        },
        Access::Store {
            window: SOUTH_WINDOW,
            offset: intr_cfg(pin),
            value: cfg,
        },
        Access::Store {
            window: SOUTH_WINDOW,
            offset: intr_status(pin),
            value: 0,
        },
        Access::Store {
            window: SOUTH_WINDOW,
            offset: intr_cfg(pin),
            value: cfg | INTR_ENABLE,
        },
    ])
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

#[test_harness::test]
fn an_output_writes_its_level_before_one_ctl_write_with_gpio_and_output_enable() {
    let blob = fdt_test::build(TREE);
    let registers = RegisterFile::new();
    registers.set(
        SOUTH_WINDOW,
        ctl(5),
        1 << FUNCTION_SHIFT | 7 << DRIVE_SHIFT | PULL_UP,
    );
    let controller = Controller::new(&blob, &registers);

    let line = controller.line("reset").expect("active-low line");
    controller.tlmm.output(&line, false);

    test_harness::check_eq!(
        registers.take_accesses(),
        [
            Access::Load {
                window: SOUTH_WINDOW,
                offset: io(5),
            },
            Access::Store {
                window: SOUTH_WINDOW,
                offset: io(5),
                value: IO_OUT,
            },
            Access::Load {
                window: SOUTH_WINDOW,
                offset: ctl(5),
            },
            Access::Store {
                window: SOUTH_WINDOW,
                offset: ctl(5),
                value: OE | 7 << DRIVE_SHIFT | PULL_UP,
            },
        ]
    );
}

#[test_harness::test]
fn an_input_leaves_another_function_for_gpio_and_keeps_its_pull_and_drive() {
    let blob = fdt_test::build(TREE);
    let registers = RegisterFile::new();
    registers.set(
        SOUTH_WINDOW,
        ctl(2),
        OE | 3 << FUNCTION_SHIFT | 7 << DRIVE_SHIFT | PULL_UP,
    );
    let controller = Controller::new(&blob, &registers);

    let line = controller.line("enable").expect("active-high line");
    controller.tlmm.input(&line);

    test_harness::check_eq!(stores(&registers.take_accesses()), [(SOUTH_WINDOW, ctl(2))]);
    test_harness::check_eq!(
        registers.get(SOUTH_WINDOW, ctl(2)),
        7 << DRIVE_SHIFT | PULL_UP
    );
}

#[test_harness::test]
fn an_active_low_line_reads_and_drives_its_level_inverted() {
    let blob = fdt_test::build(TREE);
    let registers = RegisterFile::new();
    let controller = Controller::new(&blob, &registers);
    let line = controller.line("reset").expect("active-low line");

    registers.set(SOUTH_WINDOW, io(5), IO_IN);
    let high = controller.tlmm.active(&line);
    registers.set(SOUTH_WINDOW, io(5), 0);
    let low = controller.tlmm.active(&line);
    controller.tlmm.set(&line, true);
    let asserted = registers.get(SOUTH_WINDOW, io(5));
    controller.tlmm.set(&line, false);
    let released = registers.get(SOUTH_WINDOW, io(5));

    test_harness::check_eq!([high, low], [false, true]);
    test_harness::check_eq!([asserted, released], [0, IO_OUT]);
}

#[test_harness::test]
fn a_refused_line_reads_and_writes_no_register() {
    let blob = fdt_test::build(TREE);
    let registers = RegisterFile::new();
    let controller = Controller::new(&blob, &registers);
    let outside = controller.tlmm.line(
        &controller.description,
        &device(PANEL),
        &device(I2C_HOST),
        "",
        0,
    );

    let refusals =
        ["foreign", "odd", "protected", "beyond"].map(|name| controller.line(name).map(drop));

    test_harness::check_eq!(outside.map(drop), Err(PinsError::NoDevice));
    test_harness::check_eq!(
        refusals,
        [
            Err(PinsError::Unsupported),
            Err(PinsError::Unsupported),
            Err(PinsError::Denied),
            Err(PinsError::Unsupported),
        ]
    );
    test_harness::check_eq!(registers.take_accesses(), []);
}

#[test_harness::test]
fn an_enabled_interrupt_takes_its_trigger_target_and_raw_status_then_clears_its_latch() {
    let blob = fdt_test::build(TREE);
    let registers = RegisterFile::new();
    let controller = Controller::new(&blob, &registers);
    let rising = 1 << DETECTION_SHIFT | INTR_POLARITY;
    let falling = 2 << DETECTION_SHIFT | INTR_POLARITY;
    let both = 3 << DETECTION_SHIFT | INTR_POLARITY;

    let triggers = [rising, falling, both, INTR_POLARITY, 0];

    let configured: Vec<_> = (0..5)
        .map(|index| {
            registers.set(SOUTH_WINDOW, intr_cfg(5), INTR_ENABLE | both);
            let line = controller.interrupt(index).expect("interrupt line");
            controller.tlmm.enable_irq(&line);
            registers.take_accesses()
        })
        .collect();
    let input = controller.line("enable").expect("input line");
    controller.tlmm.enable_irq(&input);
    let input_accesses = registers.take_accesses();

    test_harness::check_eq!(
        configured,
        triggers.map(|trigger| armed(5, trigger)).to_vec()
    );
    test_harness::check_eq!(input_accesses, armed(2, both));
}

#[test_harness::test]
fn a_held_edge_keeps_raw_status_after_its_latch_and_a_held_level_drops_it_first() {
    let blob = fdt_test::build(TREE);
    let registers = RegisterFile::new();
    let controller = Controller::new(&blob, &registers);
    let edge = controller.interrupt(0).expect("rising line");
    let level = controller.interrupt(3).expect("high line");
    let rising = TARGET_APPS | RAW_STATUS_EN | 1 << DETECTION_SHIFT | INTR_POLARITY;
    let high = TARGET_APPS | RAW_STATUS_EN | INTR_POLARITY;

    registers.set(SOUTH_WINDOW, intr_cfg(5), rising | INTR_ENABLE);
    controller.tlmm.hold_irq(&edge);
    let edge_accesses = registers.take_accesses();
    registers.set(SOUTH_WINDOW, intr_cfg(5), high | INTR_ENABLE);
    controller.tlmm.hold_irq(&level);
    let level_accesses = registers.take_accesses();

    test_harness::check_eq!(
        edge_accesses,
        [
            Access::Store {
                window: SOUTH_WINDOW,
                offset: intr_status(5),
                value: 0,
            },
            Access::Load {
                window: SOUTH_WINDOW,
                offset: intr_cfg(5),
            },
            Access::Store {
                window: SOUTH_WINDOW,
                offset: intr_cfg(5),
                value: rising,
            },
        ]
    );
    test_harness::check_eq!(
        level_accesses,
        [
            Access::Load {
                window: SOUTH_WINDOW,
                offset: intr_cfg(5),
            },
            Access::Store {
                window: SOUTH_WINDOW,
                offset: intr_cfg(5),
                value: high & !RAW_STATUS_EN,
            },
            Access::Store {
                window: SOUTH_WINDOW,
                offset: intr_status(5),
                value: 0,
            },
        ]
    );
}

#[test_harness::test]
fn an_ack_enables_a_held_level_and_leaves_its_latch() {
    let blob = fdt_test::build(TREE);
    let registers = RegisterFile::new();
    let controller = Controller::new(&blob, &registers);
    let level = controller.interrupt(3).expect("high line");
    registers.set(SOUTH_WINDOW, intr_cfg(5), TARGET_APPS | INTR_POLARITY);

    controller.tlmm.ack_irq(&level);

    test_harness::check_eq!(
        stores(&registers.take_accesses()),
        [(SOUTH_WINDOW, intr_cfg(5))]
    );
    test_harness::check_eq!(
        registers.get(SOUTH_WINDOW, intr_cfg(5)),
        TARGET_APPS | RAW_STATUS_EN | INTR_POLARITY | INTR_ENABLE
    );
}

#[test_harness::test]
fn a_pin_fires_only_while_latched_with_its_interrupt_enabled() {
    let blob = fdt_test::build(TREE);
    let registers = RegisterFile::new();
    let controller = Controller::new(&blob, &registers);
    let edge = controller.interrupt(0).expect("rising line");
    registers.set(SOUTH_WINDOW, intr_status(5), INTR_LATCHED);

    registers.set(SOUTH_WINDOW, intr_cfg(5), RAW_STATUS_EN);
    let held = controller.tlmm.irq_fired(&edge);
    let held_accesses = registers.take_accesses();
    registers.set(SOUTH_WINDOW, intr_cfg(5), RAW_STATUS_EN | INTR_ENABLE);
    let enabled = controller.tlmm.irq_fired(&edge);
    registers.set(SOUTH_WINDOW, intr_status(5), 0);
    let unlatched = controller.tlmm.irq_fired(&edge);

    test_harness::check_eq!([held, enabled, unlatched], [false, true, false]);
    test_harness::check_eq!(
        held_accesses,
        [Access::Load {
            window: SOUTH_WINDOW,
            offset: intr_cfg(5),
        }]
    );
}

#[test_harness::test]
fn a_refused_interrupt_record_reads_and_writes_no_register() {
    let blob = fdt_test::build(TREE);
    let registers = RegisterFile::new();
    let controller = Controller::new(&blob, &registers);
    let outside = controller.tlmm.interrupt(
        &controller.description,
        &device(PANEL),
        &device(I2C_HOST),
        0,
    );

    let refusals = [5, 6, 7, 8, 9].map(|index| controller.interrupt(index).map(drop));

    test_harness::check_eq!(outside.map(drop), Err(PinsError::NoDevice));
    test_harness::check_eq!(
        refusals,
        [
            Err(PinsError::Unsupported),
            Err(PinsError::Denied),
            Err(PinsError::Unsupported),
            Err(PinsError::Unsupported),
            Err(PinsError::NoDevice),
        ]
    );
    test_harness::check_eq!(registers.take_accesses(), []);
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

    let sdc = client.open(device(BOARD_SDC_HOST)).map(drop);
    let no_state = client.open(device(BOARD_NO_STATE)).map(drop);

    test_harness::check_eq!(unfiltered, Err(CallError::Remote(SessionError::Unbound)));
    test_harness::check_eq!(sdc, Err(CallError::Remote(PinsError::Unsupported)));
    test_harness::check_eq!(no_state, Err(CallError::Remote(PinsError::NoDevice)));

    finish(composition);
}
