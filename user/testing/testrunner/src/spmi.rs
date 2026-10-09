//! Разбор устройств и команды арбитра `spmi-pmic-arb` на модели регистров и клиент `Spmi`
//! часов PMIC на описанном арбитре.

use alloc::{collections::BTreeMap, string::String, vec::Vec};
use core::cell::{Cell, RefCell};

use component_abi::RequirementDeclaration;
use fdt_test::Token;
use ipc::{CallError, Protocol};
use kernel_objects::{Error, Session, SessionTransport, Signal};
use platform_abi::{Description, DeviceId};
use runtime::Timeout;
use spmi_abi::{DeviceError, SpmiError, spmi, spmi_device};
use spmi_pmic_arb::{
    Arbiter, ArbiterNode, Interrupt, Peripheral, Refusal, Registers, SkipReason, Skipped, Span,
    Trigger, Window,
};
use syscall::{SIGNALED, SyscallError};

use crate::{
    composition::{Importer, pause, request},
    rootkeeper_system::{
        POLL_ATTEMPTS, POLL_INTERVAL, await_revoked, finish, platform_describes, with_drivers,
    },
};

const ARBITER: &str = "/spmi@0";

const TREE: &[Token<'static>] = &fdt_test::tree! {
    spmi@0 {
        compatible = "qcom,spmi-pmic-arb";
        "reg-names" = "core", "chnls", "obsrvr", "intr", "cnfg";
        "qcom,ee" = <0>;
        #address-cells = <2>;
        #size-cells = <0>;
        interrupt-controller;
        #interrupt-cells = <4>;
        phandle = <1>;
        pmic@0 {
            compatible = "qcom,pm660", "qcom,spmi-pmic";
            reg = <0 0>;
            #address-cells = <1>;
            #size-cells = <0>;
            rtc@6000 {
                compatible = "qcom,pm8941-rtc";
                reg = <0x6000 0x6100>;
                interrupts = <0 0x61 1 1 0 0x08 0 1 0 0x60 9 1 0 0x60 0 5 2 0x60 0 1>;
            };
            misaligned@6010 {
                compatible = "test,misaligned";
                reg = <0x6010>;
            };
            foreign@7000 {
                compatible = "test,foreign";
                reg = <0x7000>;
                interrupt-parent = <2>;
                interrupts = <0 0x70 0 1>;
            };
        };
        pmic@2 {
            compatible = "qcom,pm660l", "qcom,spmi-pmic";
            reg = <2 0>;
            #address-cells = <1>;
            #size-cells = <0>;
            led@d000 {
                compatible = "test,led";
                reg = <0xd000>;
                interrupts = <2 0xd0 3 8>;
            };
        };
    };
};

const PARTIAL_CELLS: &[Token<'static>] = &[
    Token::Node("spmi@0"),
    Token::Strings("compatible", &["qcom,spmi-pmic-arb"]),
    Token::Strings("reg-names", &["core", "chnls", "obsrvr", "intr", "cnfg"]),
    Token::Cells("qcom,ee", &[0]),
    Token::Cells("#interrupt-cells", &[4]),
    Token::Node("pmic@0"),
    Token::Cells("reg", &[0, 0]),
    Token::Cells("#address-cells", &[1]),
    Token::Cells("#size-cells", &[0]),
    Token::Node("reg@6000"),
    Token::Strings("compatible", &["test,partial-reg"]),
    Token::Bytes("reg", &[0, 0, 0x60, 0, 0]),
    Token::End,
    Token::Node("interrupts@6100"),
    Token::Strings("compatible", &["test,partial-interrupts"]),
    Token::Cells("reg", &[0x6100]),
    Token::Bytes(
        "interrupts",
        &[0, 0, 0, 0, 0, 0, 0, 0x61, 0, 0, 0, 1, 0, 0, 0, 1, 0],
    ),
    Token::End,
    Token::End,
    Token::End,
];

const VERSION_3: u32 = 0x3000_0000;
const VERSION_2: u32 = 0x2001_0000;
const VERSION_5: u32 = 0x5000_0000;

const CHANNELS: [(usize, u32, u32); 3] = [(0, 0x060, 0), (1, 0x061, 0), (2, 0x2D0, 1)];

const CHANNEL_STRIDE: usize = 0x8000;
const EE_STRIDE: usize = 0x1000;
const STATUS: usize = 0x8;
const WRITE_DATA: usize = 0x10;
const READ_DATA: usize = 0x18;

const STATUS_DONE: u32 = 1;
const STATUS_FAILURE: u32 = 1 << 1;
const STATUS_DENIED: u32 = 1 << 2;

const INTERRUPT_STRIDE: usize = 0x1000;
const CHANNEL_ENABLE: usize = 0x0;
const CHANNEL_STATUS: usize = 0x4;
const CHANNEL_CLEAR: usize = 0x8;

const ACCUMULATOR_V2: usize = 0x10_0000;
const ACCUMULATOR_V3: usize = 0x20_0000;

const SET_TYPE: usize = 0x11;
const LATCHED_CLEAR: usize = 0x14;
const ENABLE_CLEAR: usize = 0x16;

const UNREQUESTED_BYTE: u8 = 0xAA;

const CLOCK_ALARM: Interrupt = Interrupt {
    ppid: 0x061,
    bit: 1,
    trigger: Trigger::Rising,
};

const PMIC_ARBITER: &str = "/soc@0/spmi@800f000";
const PMIC_CLOCK: &str = "qcom,pm8941-rtc";

const SPMI_REQUIREMENT: &str = "spmi";
const SPMI_REPLY_TIMEOUT: Timeout = Timeout::from_secs(5);

const RTC_READ: u16 = 0x6048;
const RTC_PAST_BLOCKS: u16 = 0x6200;
const ALARM_WRITE: u16 = 0x6140;
const ALARM_CONTROL: u16 = 0x6146;
const ALARM_CONTROL_2: u16 = 0x6148;

const ALARM_ENABLE: u64 = 1 << 7;
const ALARM_CLEAR: u64 = 1;

const COUNT_PAUSE: Timeout = Timeout::from_secs(2);
const ALARM_AHEAD: u64 = 2;
const ALARM_WAIT: Timeout = Timeout::from_secs(3);

const SPMI_IMPORTS: &[crate::composition::Import] = &[crate::composition::Import {
    declaration: RequirementDeclaration::all(SPMI_REQUIREMENT, spmi::Protocol::ID),
    namespace_root: None,
}];

struct Model {
    registers: RefCell<BTreeMap<(usize, usize), u32>>,
    pmic: RefCell<BTreeMap<(usize, usize), u8>>,
    status: Cell<u32>,
    commands: Cell<usize>,
    steps: RefCell<Vec<Step>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Step {
    Pmic {
        apid: usize,
        address: usize,
        value: u64,
    },
    Interrupts {
        offset: usize,
        value: u32,
    },
}

impl Model {
    fn new(version: u32) -> Self {
        let model = Self {
            registers: RefCell::new(BTreeMap::new()),
            pmic: RefCell::new(BTreeMap::new()),
            status: Cell::new(STATUS_DONE),
            commands: Cell::new(0),
            steps: RefCell::new(Vec::new()),
        };

        model.store(Window::Core, 0, version);
        for (apid, ppid, owner) in CHANNELS {
            model.store(Window::Core, 0x800 + 4 * apid, ppid << 8);
            model.store(Window::Config, 0x700 + 4 * apid, owner);
        }

        model
    }

    fn load(&self, window: Window, offset: usize) -> u32 {
        let key = (window as usize, offset);
        self.registers.borrow().get(&key).copied().unwrap_or(0)
    }

    fn store(&self, window: Window, offset: usize, value: u32) {
        self.registers
            .borrow_mut()
            .insert((window as usize, offset), value);

        if matches!(window, Window::Interrupts) {
            self.steps
                .borrow_mut()
                .push(Step::Interrupts { offset, value });
        }

        if matches!(window, Window::Channels | Window::Observer) && offset.is_multiple_of(EE_STRIDE)
        {
            self.execute(window, offset, value);
        }
    }

    fn execute(&self, window: Window, offset: usize, command: u32) {
        self.commands.set(self.commands.get() + 1);
        self.store(window, offset + STATUS, self.status.get());

        let apid = offset / CHANNEL_STRIDE;
        let address = ((command >> 4) & 0xFF) as usize;
        let count = (command & 0x7) as usize + 1;

        if command >> 27 == 1 {
            let bytes: [u8; 8] = {
                let pmic = self.pmic.borrow();
                core::array::from_fn(|index| {
                    if index < count {
                        pmic.get(&(apid, address + index)).copied().unwrap_or(0)
                    } else {
                        UNREQUESTED_BYTE
                    }
                })
            };

            let value = u64::from_le_bytes(bytes);
            self.store(window, offset + READ_DATA, value as u32);
            self.store(window, offset + READ_DATA + 4, (value >> 32) as u32);
        } else {
            let data = u64::from(self.load(window, offset + WRITE_DATA))
                | u64::from(self.load(window, offset + WRITE_DATA + 4)) << 32;

            for (index, byte) in data.to_le_bytes().into_iter().take(count).enumerate() {
                self.pmic.borrow_mut().insert((apid, address + index), byte);
            }

            self.steps.borrow_mut().push(Step::Pmic {
                apid,
                address,
                value: data & (u64::MAX >> (64 - 8 * count)),
            });
        }
    }
}

impl Registers for &Model {
    fn size(&self, window: Window) -> usize {
        match window {
            Window::Core => 0x1000,
            Window::Channels | Window::Observer => 0x100_0000,
            Window::Interrupts => 0x22_0000,
            Window::Config => 0x3000,
        }
    }

    fn load(&self, window: Window, offset: usize) -> u32 {
        Model::load(self, window, offset)
    }

    fn store(&self, window: Window, offset: usize, value: u32) {
        Model::store(self, window, offset, value);
    }
}

fn peripherals(tree: &[Token<'_>]) -> Vec<Result<Peripheral, Skipped>> {
    let blob = fdt_test::build(tree);
    let description = Description::new(&blob, 0).expect("tree parses");
    let arbiter = DeviceId::new(ARBITER).expect("absolute path");
    let node = ArbiterNode::parse(&description, &arbiter).expect("arbiter node parses");
    node.peripherals(&description, &arbiter)
}

fn peripheral(path: &str) -> Peripheral {
    peripherals(TREE)
        .into_iter()
        .filter_map(Result::ok)
        .find(|peripheral| peripheral.id().as_str() == path)
        .expect("described peripheral")
}

fn arbiter(model: &Model, ee: u8) -> Arbiter<&Model> {
    Arbiter::new(model, ee).expect("model arbiter")
}

fn drained(arbiter: &Arbiter<&Model>) -> Vec<(u16, u8)> {
    let mut delivered = Vec::new();
    arbiter.drain(|ppid, bits| delivered.push((ppid, bits)));
    delivered
}

#[test_harness::test]
fn devices_lie_below_the_pmic_nodes_with_their_sid_and_blocks() {
    let found = peripherals(TREE);
    let ids: Vec<_> = found
        .iter()
        .map(|found| match found {
            Ok(peripheral) => (String::from(peripheral.id().as_str()), None),
            Err(skipped) => (String::from(skipped.id.as_str()), Some(skipped.reason)),
        })
        .collect();
    let clock: Vec<_> = peripheral("/spmi@0/pmic@0/rtc@6000").ppids().collect();
    let led: Vec<_> = peripheral("/spmi@0/pmic@2/led@d000").ppids().collect();

    test_harness::check_eq!(
        ids,
        [
            ("/spmi@0/pmic@0/rtc@6000".into(), None),
            (
                "/spmi@0/pmic@0/misaligned@6010".into(),
                Some(SkipReason::Block)
            ),
            ("/spmi@0/pmic@0/foreign@7000".into(), None),
            ("/spmi@0/pmic@2/led@d000".into(), None),
        ]
    );
    test_harness::check_eq!(clock, [0x060, 0x061]);
    test_harness::check_eq!(led, [0x2D0]);
}

#[test_harness::test]
fn a_property_with_a_partial_cell_describes_nothing() {
    let found = peripherals(PARTIAL_CELLS);
    let reasons: Vec<_> = found
        .iter()
        .map(|found| found.as_ref().err().map(|skipped| skipped.reason))
        .collect();
    let interrupt = found
        .iter()
        .find_map(|found| found.as_ref().ok())
        .map(|peripheral| peripheral.interrupt(0));

    test_harness::check_eq!(reasons, [Some(SkipReason::Block), None]);
    test_harness::check_eq!(interrupt, Some(Err(DeviceError::NotDescribed)));
}

#[test_harness::test]
fn registers_of_a_session_lie_in_one_block_of_its_device() {
    let clock = peripheral("/spmi@0/pmic@0/rtc@6000");

    test_harness::check_eq!(
        clock.span(0x6048, 4),
        Ok(Span {
            ppid: 0x060,
            offset: 0x48
        })
    );
    test_harness::check_eq!(clock.span(0x61F8, 8).map(|span| span.ppid), Ok(0x061));
    test_harness::check_eq!(clock.span(0x60FE, 4), Err(DeviceError::OutOfRange));
    test_harness::check_eq!(clock.span(0x6200, 1), Err(DeviceError::OutOfRange));
    test_harness::check_eq!(clock.span(0x6048, 9), Err(DeviceError::OutOfRange));
    test_harness::check_eq!(clock.span(0x6048, 0), Err(DeviceError::OutOfRange));
}

#[test_harness::test]
fn an_interrupt_of_a_session_belongs_to_its_device_and_to_the_arbiter() {
    let clock = peripheral("/spmi@0/pmic@0/rtc@6000");
    let foreign = peripheral("/spmi@0/pmic@0/foreign@7000");
    let led = peripheral("/spmi@0/pmic@2/led@d000");

    test_harness::check_eq!(clock.interrupt(0), Ok(CLOCK_ALARM));
    test_harness::check_eq!(clock.interrupt(1), Err(DeviceError::OutOfRange));
    test_harness::check_eq!(clock.interrupt(2), Err(DeviceError::OutOfRange));
    test_harness::check_eq!(clock.interrupt(3), Err(DeviceError::NotDescribed));
    test_harness::check_eq!(clock.interrupt(4), Err(DeviceError::OutOfRange));
    test_harness::check_eq!(clock.interrupt(5), Err(DeviceError::NotDescribed));
    test_harness::check_eq!(foreign.interrupt(0), Err(DeviceError::NotDescribed));
    test_harness::check_eq!(led.interrupt(0).map(|irq| irq.trigger), Ok(Trigger::Low));
}

#[test_harness::test]
fn an_arbiter_outside_versions_2_and_3_is_refused() {
    let unsupported = Model::new(VERSION_5);
    let refused = Arbiter::new(&unsupported, 0).err();
    let accepted =
        [VERSION_2, VERSION_3].map(|version| arbiter(&Model::new(version), 0).channels());

    test_harness::check_eq!(refused, Some(Refusal::Version(VERSION_5)));
    test_harness::check_eq!(accepted, [512, 512]);
}

#[test_harness::test]
fn a_command_carries_the_requested_bytes_from_the_lowest_one() {
    let model = Model::new(VERSION_3);
    let arbiter = arbiter(&model, 0);

    let written = arbiter.write(0x061, 0x40, 6, 0x0605_0403_0201);
    let read = arbiter.read(0x061, 0x40, 6);
    let short = arbiter.read(0x061, 0x41, 2);

    test_harness::check_eq!(written, Ok(()));
    test_harness::check_eq!(read, Ok(0x0605_0403_0201));
    test_harness::check_eq!(short, Ok(0x0302));
}

#[test_harness::test]
fn a_write_to_a_channel_of_another_environment_is_denied_without_a_command() {
    let model = Model::new(VERSION_3);
    let arbiter = arbiter(&model, 0);

    let denied = arbiter.write(0x2D0, 0x46, 1, 0x80);
    let commands = model.commands.get();
    let read = arbiter.read(0x2D0, 0x46, 1);
    let absent = arbiter.read(0x3D0, 0x46, 1);

    test_harness::check_eq!(denied, Err(DeviceError::Denied));
    test_harness::check_eq!(commands, 0);
    test_harness::check_eq!(read, Ok(0));
    test_harness::check_eq!(absent, Err(DeviceError::Failed));
}

#[test_harness::test]
fn the_status_of_a_command_names_its_outcome() {
    let outcomes = [STATUS_DONE | STATUS_DENIED, STATUS_DONE | STATUS_FAILURE, 0].map(|status| {
        let model = Model::new(VERSION_3);
        model.status.set(status);
        arbiter(&model, 0).read(0x060, 0x48, 4)
    });

    test_harness::check_eq!(
        outcomes,
        [
            Err(DeviceError::Denied),
            Err(DeviceError::Failed),
            Err(DeviceError::Failed)
        ]
    );
}

#[test_harness::test]
fn an_accumulated_channel_is_cleared_masked_and_delivered() {
    for (version, accumulator) in [(VERSION_2, ACCUMULATOR_V2), (VERSION_3, ACCUMULATOR_V3)] {
        let model = Model::new(version);
        let arbiter = arbiter(&model, 0);
        let armed = arbiter.arm(&CLOCK_ALARM);

        model.store(Window::Interrupts, INTERRUPT_STRIDE + CHANNEL_STATUS, 0b10);
        model.store(Window::Interrupts, accumulator, 0b10);
        model.steps.take();

        let delivered = drained(&arbiter);
        let steps = model.steps.take();

        test_harness::check_eq!(armed, Ok(()));
        test_harness::check_eq!(delivered, [(0x061, 0b10)]);
        test_harness::check_eq!(
            steps,
            [
                Step::Pmic {
                    apid: 1,
                    address: ENABLE_CLEAR,
                    value: 0b10
                },
                Step::Interrupts {
                    offset: INTERRUPT_STRIDE + CHANNEL_CLEAR,
                    value: 0b10
                },
            ]
        );
    }
}

#[test_harness::test]
fn an_empty_accumulator_falls_back_to_the_enabled_channels_of_the_environment() {
    let model = Model::new(VERSION_3);
    let arbiter = arbiter(&model, 0);
    let clock = Interrupt {
        ppid: 0x060,
        bit: 0,
        trigger: Trigger::High,
    };
    let armed = arbiter.arm(&clock);

    model.store(Window::Interrupts, CHANNEL_STATUS, 0b1);
    model.store(Window::Interrupts, INTERRUPT_STRIDE + CHANNEL_STATUS, 0b1);

    let delivered = drained(&arbiter);
    let set_type = arbiter.read(0x060, 0x11, 3);

    test_harness::check_eq!(armed, Ok(()));
    test_harness::check_eq!(delivered, [(0x060, 0b1)]);
    test_harness::check_eq!(set_type, Ok(0x00_01_00));
}

#[test_harness::test]
fn arming_masks_and_unlatches_the_interrupt_before_it_enables_the_channel_and_the_block() {
    let model = Model::new(VERSION_3);
    let arbiter = arbiter(&model, 0);

    let armed = arbiter.arm(&CLOCK_ALARM);
    let steps = model.steps.take();

    test_harness::check_eq!(armed, Ok(()));
    test_harness::check_eq!(
        steps,
        [
            Step::Pmic {
                apid: 1,
                address: ENABLE_CLEAR,
                value: 0b10
            },
            Step::Pmic {
                apid: 1,
                address: LATCHED_CLEAR,
                value: 0b10
            },
            Step::Pmic {
                apid: 1,
                address: SET_TYPE,
                value: 0x00_02_02
            },
            Step::Interrupts {
                offset: INTERRUPT_STRIDE + CHANNEL_ENABLE,
                value: 1
            },
            Step::Pmic {
                apid: 1,
                address: LATCHED_CLEAR,
                value: 0x02_02
            },
        ]
    );
}

#[test_harness::test]
fn an_arbiter_of_another_environment_uses_its_command_page_and_accumulator() {
    let model = Model::new(VERSION_3);
    let arbiter = arbiter(&model, 1);
    let led = Interrupt {
        ppid: 0x2D0,
        bit: 3,
        trigger: Trigger::Low,
    };

    let written = arbiter.write(0x2D0, 0x46, 1, 0x80);
    let command = model.load(Window::Channels, 2 * CHANNEL_STRIDE + EE_STRIDE);
    let denied = arbiter.write(0x060, 0x46, 1, 0x80);
    let armed = arbiter.arm(&led);

    model.store(Window::Interrupts, CHANNEL_ENABLE, 1);
    model.store(Window::Interrupts, CHANNEL_STATUS, 0b1);
    model.store(Window::Interrupts, ACCUMULATOR_V3, 0b1);

    model.store(
        Window::Interrupts,
        2 * INTERRUPT_STRIDE + CHANNEL_STATUS,
        0b1000,
    );
    model.store(Window::Interrupts, ACCUMULATOR_V3 + EE_STRIDE, 0b100);

    let delivered = drained(&arbiter);

    test_harness::check_eq!(written, Ok(()));
    test_harness::check_eq!(command, 0x46 << 4);
    test_harness::check_eq!(denied, Err(DeviceError::Denied));
    test_harness::check_eq!(armed, Ok(()));
    test_harness::check_eq!(delivered, [(0x2D0, 0b1000)]);
}

fn spmi_session(importer: &Importer) -> spmi::Client<SessionTransport> {
    request::<spmi::Protocol>(importer.handle(), SPMI_REQUIREMENT, SPMI_REPLY_TIMEOUT)
        .expect("spmi session")
        .client()
        .with_wait(SPMI_REPLY_TIMEOUT)
}

fn open(
    bus: &spmi::Client<SessionTransport>,
) -> Result<spmi_device::Client<SessionTransport>, CallError<SpmiError>> {
    let compatible = PMIC_CLOCK;
    bus.open(compatible).map(|session| {
        Session::<spmi_device::Protocol>::from_session(session)
            .client()
            .with_wait(SPMI_REPLY_TIMEOUT)
    })
}

fn update(
    clock: &spmi_device::Client<SessionTransport>,
    address: u16,
    set: u64,
    clear: u64,
) -> Result<(), CallError<DeviceError>> {
    let value = clock.read(address, 1)?;
    clock.write(address, 1, (value | set) & !clear)
}

#[test_harness::test]
fn a_pmic_clock_counts_and_its_alarm_raises_the_signal_of_its_session() {
    if !platform_describes(PMIC_ARBITER) {
        test_harness::skip!("{} is not described", PMIC_ARBITER);
    }

    let composition = with_drivers();
    let importer = composition.importer(SPMI_IMPORTS);
    let bus = spmi_session(&importer);
    let clock = open(&bus).expect("pmic clock session");

    let first = clock.read(RTC_READ, 4).expect("count read");
    pause(COUNT_PAUSE);
    let second = clock.read(RTC_READ, 4).expect("count read");

    let busy = open(&bus).map(drop);
    let past_blocks = clock.write(RTC_PAST_BLOCKS, 1, 0);

    let alarm = clock.irq(0).map(Signal::from);
    let set = update(&clock, ALARM_CONTROL, 0, ALARM_ENABLE)
        .and_then(|()| clock.write(ALARM_WRITE, 4, second + ALARM_AHEAD))
        .and_then(|()| update(&clock, ALARM_CONTROL, ALARM_ENABLE, 0));
    let raised = alarm
        .as_ref()
        .is_ok_and(|alarm| set.is_ok() && alarm.wait(SIGNALED, ALARM_WAIT).is_ok());

    let cleared = update(&clock, ALARM_CONTROL, 0, ALARM_ENABLE)
        .and_then(|()| update(&clock, ALARM_CONTROL_2, 0, ALARM_CLEAR));
    let acknowledged = clock.ack(0);
    let lowered = alarm
        .as_ref()
        .map(|alarm| alarm.wait(SIGNALED, Timeout::from_millis(0)))
        .ok();

    drop(clock);

    let revoked = alarm
        .as_ref()
        .is_ok_and(|alarm| await_revoked(|| alarm.wait(SIGNALED, Timeout::from_millis(0))));

    let reopened = (0..POLL_ATTEMPTS).any(|_| match open(&bus) {
        Err(CallError::Remote(SpmiError::Busy)) => {
            pause(POLL_INTERVAL);
            false
        }
        opened => opened.is_ok(),
    });

    test_harness::check!((1..=3).contains(&second.wrapping_sub(first)));
    test_harness::check_eq!(busy, Err(CallError::Remote(SpmiError::Busy)));
    test_harness::check_eq!(past_blocks, Err(CallError::Remote(DeviceError::OutOfRange)));
    test_harness::check!(alarm.is_ok());
    test_harness::check_eq!(set, Ok(()));
    test_harness::check!(raised);
    test_harness::check_eq!(cleared, Ok(()));
    test_harness::check_eq!(acknowledged, Ok(()));
    test_harness::check_eq!(lowered, Some(Err(Error::Syscall(SyscallError::Timeout))));
    test_harness::check!(revoked);
    test_harness::check!(reopened);

    finish(composition);
}
