//! Такты узлов контроллером GCC `clk-qcom` на модели регистров и клиент `Clocks` контроллера
//! платы.

use alloc::{string::String, vec::Vec};

use clk_qcom::{Branch, Clock, Enable, Frequency, Gcc, Generator};
use clocks_abi::{ClocksError, clocks};
use component_abi::{RequirementDeclaration, SessionError};
use fdt_test::Token;
use ipc::{CallError, Protocol};
use mmio::Registers;
use platform_abi::{Description, DeviceClaims, DeviceId};
use runtime::Timeout;

use crate::{
    composition::{Import, compatibles, controller_provider, device, request},
    register_model::{Access, RegisterFile},
    rootkeeper_system::{finish, platform_describes, with_description, with_drivers},
};

const CONTROLLER: &str = "/soc@0/clock-controller@100000";

const WINDOW: usize = 0;
const WINDOW_SIZE: usize = 0x9_4000;

const VOTE: usize = 0x52004;
const BLSP1_AHB_VOTE: u32 = 1 << 17;
const BLSP1_AHB_HALT: usize = 0x17004;
const QUP1_BRANCH: usize = 0x19008;
const QUP2_BRANCH: usize = 0x1b008;
const QUP1_COMMAND: usize = 0x19020;
const QUP1_CFG: usize = 0x19024;
const QUP2_COMMAND: usize = 0x1b020;

const BRANCH_ENABLE: u32 = 1 << 0;
const CLK_OFF: u32 = 1 << 31;
const UPDATE: u32 = 1 << 0;
const SOURCE_SHIFT: u32 = 8;
const MODE_SHIFT: u32 = 12;
const HW_CLK_CTRL: u32 = 1 << 20;

const XO_HZ: u64 = 19_200_000;
const QUP_GENERATOR_OFFSET: usize = 0x18;

const QUP_FREQUENCIES: &[Frequency] = &[Frequency {
    hz: XO_HZ,
    source: 0,
    divider: 1,
}];

const CLOCKS: &[Clock] = &[
    Clock {
        index: 25,
        branch: Branch {
            enable: Enable::Vote(BLSP1_AHB_VOTE),
            halt: BLSP1_AHB_HALT,
            generator: None,
        },
    },
    qup(26, QUP1_BRANCH),
    qup(28, QUP2_BRANCH),
];

const QUP1_HOST: &str = "/soc@0/i2c@c175000";
const QUP2_HOST: &str = "/soc@0/i2c@c176000";
const DISABLED_HOST: &str = "/soc@0/i2c@c177000";
const ABSENT_HOST: &str = "/soc@0/i2c@c178000";
const FIXED_ONLY_HOST: &str = "/soc@0/timer@1000";
const UART_HOST: &str = "/soc@0/serial@c170000";

const TREE: &[Token<'static>] = &fdt_test::tree! {
    xo-board {
        compatible = "fixed-clock";
        #clock-cells = <0>;
        phandle = <2>;
    };
    soc@0 {
        clock-controller@100000 {
            compatible = "test,clock-controller";
            #clock-cells = <1>;
            phandle = <1>;
        };
        i2c@c175000 {
            clocks = <1 26 1 25>;
            clock-names = "core", "iface";
        };
        i2c@c176000 {
            clocks = <1 28 1 25>;
            clock-names = "core", "iface";
        };
        i2c@c177000 {
            status = "disabled";
            clocks = <1 30 1 25>;
            clock-names = "core", "iface";
        };
        timer@1000 {
            clocks = <2>;
        };
        serial@c170000 {
            clocks = <1 35 1 25>;
            clock-names = "core", "iface";
        };
    };
};

const BOARD_CONTROLLER: &str = "/soc@0/clock-controller@100000";
const BOARD_DMA: &str = "/soc@0/dma-controller@c144000";
const BOARD_QUP1: &str = "/soc@0/i2c@c175000";
const BOARD_CONSOLE: &str = "/soc@0/serial@c170000";

const CLOCKS_REQUIREMENT: &str = "clocks";
const CLOCKS_REPLY_TIMEOUT: Timeout = Timeout::from_secs(5);

const CLOCKS_IMPORTS: &[Import] = &[Import {
    declaration: RequirementDeclaration::all(CLOCKS_REQUIREMENT, clocks::Protocol::ID),
    namespace_root: None,
}];

struct Model<'a> {
    registers: &'a RegisterFile,
    stuck: Option<usize>,
}

struct Controller<'a> {
    description: Description<'a>,
    gcc: Gcc<Model<'a>>,
    claims: DeviceClaims,
}

impl Registers<()> for Model<'_> {
    fn size(&self, (): ()) -> usize {
        WINDOW_SIZE
    }

    fn load(&self, (): (), offset: usize) -> u32 {
        // Ожидание отказа опрашивает застрявший регистр тысячи раз, эти чтения журнал не пишет.
        if self.stuck == Some(offset) {
            return self.registers.get(WINDOW, offset);
        }
        self.registers.load(WINDOW, offset)
    }

    fn store(&self, (): (), offset: usize, value: u32) {
        self.registers.store(WINDOW, offset, value);
        if self.stuck == Some(offset) {
            return;
        }

        if offset == VOTE && value & BLSP1_AHB_VOTE != 0 {
            self.clear(BLSP1_AHB_HALT, CLK_OFF);
        } else if [QUP1_BRANCH, QUP2_BRANCH].contains(&offset) && value & BRANCH_ENABLE != 0 {
            self.clear(offset, CLK_OFF);
        } else if [QUP1_COMMAND, QUP2_COMMAND].contains(&offset) {
            self.clear(offset, UPDATE);
        }
    }
}

impl Model<'_> {
    fn clear(&self, offset: usize, bits: u32) {
        let value = self.registers.get(WINDOW, offset);
        self.registers.set(WINDOW, offset, value & !bits);
    }
}

impl<'a> Controller<'a> {
    fn new(blob: &'a [u8], registers: &'a RegisterFile, stuck: Option<usize>) -> Self {
        for halt in [BLSP1_AHB_HALT, QUP1_BRANCH, QUP2_BRANCH] {
            registers.set(WINDOW, halt, CLK_OFF);
        }
        let description = Description::new(blob, 0).expect("tree parses");
        let id = device(CONTROLLER);
        let model = Model { registers, stuck };
        let gcc = Gcc::new(model, CLOCKS, id).expect("model controller");

        Self {
            description,
            gcc,
            claims: DeviceClaims::new(),
        }
    }

    fn enable(&mut self, badge: u64, path: &str) -> Result<(), ClocksError> {
        let device = device(path);
        self.gcc
            .enable(&self.description, &mut self.claims, badge, &device)
    }

    fn set_rate(&self, badge: u64, name: &str, hz: u64) -> Result<(), ClocksError> {
        let device = device(QUP1_HOST);
        self.gcc
            .set_rate(&self.description, &self.claims, badge, &device, name, hz)
    }
}

const fn qup(index: u32, branch: usize) -> Clock {
    Clock {
        index,
        branch: Branch {
            enable: Enable::Branch(branch),
            halt: branch,
            generator: Some(Generator {
                command: branch + QUP_GENERATOR_OFFSET,
                frequencies: QUP_FREQUENCIES,
            }),
        },
    }
}

fn stores(accesses: &[Access]) -> Vec<(usize, u32)> {
    accesses
        .iter()
        .filter_map(|access| match *access {
            Access::Store { offset, value, .. } => Some((offset, value)),
            Access::Load { .. } => None,
        })
        .collect()
}

const fn load(offset: usize) -> Access {
    Access::Load {
        window: WINDOW,
        offset,
    }
}

const fn store(offset: usize, value: u32) -> Access {
    Access::Store {
        window: WINDOW,
        offset,
        value,
    }
}

#[test_harness::test]
fn qup1_clocks_answer_after_the_branch_and_the_vote_report_running() {
    let blob = fdt_test::build(TREE);
    let registers = RegisterFile::new();
    let mut controller = Controller::new(&blob, &registers, None);

    let opened = controller.enable(1, QUP1_HOST);

    test_harness::check_eq!(opened, Ok(()));
    test_harness::check_eq!(
        registers.take_accesses(),
        [
            load(QUP1_BRANCH),
            store(QUP1_BRANCH, CLK_OFF | BRANCH_ENABLE),
            load(QUP1_BRANCH),
            load(VOTE),
            store(VOTE, BLSP1_AHB_VOTE),
            load(BLSP1_AHB_HALT),
        ]
    );
}

#[test_harness::test]
fn a_node_of_another_session_is_busy_and_its_rate_is_not_set() {
    let blob = fdt_test::build(TREE);
    let registers = RegisterFile::new();
    let mut controller = Controller::new(&blob, &registers, None);

    let first = controller.enable(1, QUP1_HOST);
    registers.take_accesses();
    let again = controller.enable(1, QUP1_HOST);
    let other = controller.enable(2, QUP1_HOST);
    let foreign_rate = controller.set_rate(2, "core", XO_HZ);
    let refused_accesses = registers.take_accesses();
    let shared_clock = controller.enable(2, QUP2_HOST);

    test_harness::check_eq!(first, Ok(()));
    test_harness::check_eq!(again, Ok(()));
    test_harness::check_eq!(other, Err(ClocksError::Busy));
    test_harness::check_eq!(foreign_rate, Err(ClocksError::NoDevice));
    test_harness::check_eq!(refused_accesses, []);
    test_harness::check_eq!(shared_clock, Ok(()));
}

#[test_harness::test]
fn a_refused_node_leaves_registers_untouched() {
    let blob = fdt_test::build(TREE);
    let registers = RegisterFile::new();
    let mut controller = Controller::new(&blob, &registers, None);
    let refusals = [
        (DISABLED_HOST, ClocksError::NoDevice),
        (ABSENT_HOST, ClocksError::NoDevice),
        (FIXED_ONLY_HOST, ClocksError::NoDevice),
        (UART_HOST, ClocksError::Unsupported),
    ];

    let outcomes: Vec<_> = refusals
        .iter()
        .map(|(host, _)| {
            let enabled = controller.enable(1, host);
            (*host, enabled, registers.take_accesses())
        })
        .collect();

    let expected: Vec<_> = refusals
        .iter()
        .map(|(host, refusal)| (*host, Err(*refusal), Vec::new()))
        .collect();
    test_harness::check_eq!(outcomes, expected);
}

#[test_harness::test]
fn the_xo_rate_selects_source_zero_divider_one_and_waits_for_update() {
    let blob = fdt_test::build(TREE);
    let registers = RegisterFile::new();
    registers.set(
        WINDOW,
        QUP1_CFG,
        1 << SOURCE_SHIFT | 2 << MODE_SHIFT | HW_CLK_CTRL | 9,
    );
    let mut controller = Controller::new(&blob, &registers, None);
    controller.enable(1, QUP1_HOST).expect("enable");
    registers.take_accesses();

    let set = controller.set_rate(1, "core", XO_HZ);

    test_harness::check_eq!(set, Ok(()));
    test_harness::check_eq!(
        stores(&registers.take_accesses()),
        [(QUP1_CFG, 1), (QUP1_COMMAND, UPDATE)]
    );
    test_harness::check_eq!(registers.get(WINDOW, QUP1_COMMAND), 0);
}

#[test_harness::test]
fn a_rate_outside_the_table_a_clock_without_generator_and_an_unknown_name_write_nothing() {
    let blob = fdt_test::build(TREE);
    let registers = RegisterFile::new();
    let mut controller = Controller::new(&blob, &registers, None);
    controller.enable(1, QUP1_HOST).expect("enable");
    registers.take_accesses();

    let outcomes = [
        controller.set_rate(1, "core", 400_000),
        controller.set_rate(1, "iface", XO_HZ),
        controller.set_rate(1, "bus", XO_HZ),
    ];

    test_harness::check_eq!(outcomes, [Err(ClocksError::Unsupported); 3]);
    test_harness::check_eq!(stores(&registers.take_accesses()), []);
}

#[test_harness::test]
fn a_branch_that_stays_off_times_out_and_leaves_the_node_free() {
    let blob = fdt_test::build(TREE);
    let registers = RegisterFile::new();
    let mut controller = Controller::new(&blob, &registers, Some(QUP1_BRANCH));

    let first = controller.enable(1, QUP1_HOST);
    let second = controller.enable(2, QUP1_HOST);

    test_harness::check_eq!(first, Err(ClocksError::Timeout));
    test_harness::check_eq!(second, Err(ClocksError::Timeout));
    test_harness::check_eq!(registers.get(WINDOW, QUP1_BRANCH), CLK_OFF | BRANCH_ENABLE);
}

#[test_harness::test]
fn a_generator_that_keeps_update_times_out_and_keeps_the_node() {
    let blob = fdt_test::build(TREE);
    let registers = RegisterFile::new();
    let mut controller = Controller::new(&blob, &registers, Some(QUP1_COMMAND));
    controller.enable(1, QUP1_HOST).expect("enable");

    let set = controller.set_rate(1, "core", XO_HZ);

    test_harness::check_eq!(set, Err(ClocksError::Timeout));
    test_harness::check_eq!(controller.claims.holder(&device(QUP1_HOST)), Some(1));
}

#[test_harness::test]
fn clocks_stay_enabled_after_release_and_the_node_enables_again() {
    let blob = fdt_test::build(TREE);
    let registers = RegisterFile::new();
    let mut controller = Controller::new(&blob, &registers, None);
    controller.enable(1, QUP1_HOST).expect("enable");

    controller.claims.release(1);
    let again = controller.enable(2, QUP1_HOST);

    test_harness::check_eq!(registers.get(WINDOW, VOTE) & BLSP1_AHB_VOTE, BLSP1_AHB_VOTE);
    test_harness::check_eq!(
        registers.get(WINDOW, QUP1_BRANCH) & BRANCH_ENABLE,
        BRANCH_ENABLE
    );
    test_harness::check_eq!(again, Ok(()));
}

fn controller_compatibles(description: &Description<'_>, path: &str) -> Vec<String> {
    let controller = DeviceId::new(path)
        .and_then(|id| description.node(&id))
        .and_then(|node| node.specifiers("clocks", "#clock-cells"))
        .and_then(|mut records| records.next())
        .and_then(Result::ok)
        .and_then(|record| description.find(record.node.as_str()));
    compatibles(controller)
}

#[test_harness::test]
fn the_board_controller_enables_the_dma_clock_and_refuses_qup_and_console() {
    if !platform_describes(BOARD_CONTROLLER) {
        test_harness::skip!("{} is not described", BOARD_CONTROLLER);
    }

    let controller = with_description(|description| controller_compatibles(description, BOARD_DMA));

    let composition = with_drivers();
    let importer = composition.importer(CLOCKS_IMPORTS);
    let unfiltered =
        request::<clocks::Protocol>(importer.handle(), CLOCKS_REQUIREMENT, CLOCKS_REPLY_TIMEOUT)
            .map(drop);

    let client =
        controller_provider::<clocks::Protocol>(importer.handle(), CLOCKS_REQUIREMENT, &controller)
            .client()
            .with_wait(CLOCKS_REPLY_TIMEOUT);

    let enable = |path| client.enable(device(path));
    let dma = enable(BOARD_DMA);
    let qup = enable(BOARD_QUP1);
    let console = enable(BOARD_CONSOLE);

    test_harness::check_eq!(unfiltered, Err(CallError::Remote(SessionError::Unbound)));
    test_harness::check_eq!(dma, Ok(()));
    test_harness::check_eq!(qup, Err(CallError::Remote(ClocksError::NoDevice)));
    test_harness::check_eq!(console, Err(CallError::Remote(ClocksError::Unsupported)));

    finish(composition);
}
