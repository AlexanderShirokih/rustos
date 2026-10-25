//! Устройства и передачи шины `i2c-qup` на модели контроллера QUP и клиент `I2c` шины платы.

use alloc::{
    collections::VecDeque,
    string::{String, ToString},
    vec,
    vec::Vec,
};
use core::{
    cell::{Cell, RefCell},
    task::{Context, Poll, Waker},
};

use component_abi::{RequirementDeclaration, SessionError};
use driver_novatek_nvt_ts::{PARAMETERS_ADDRESS, PARAMETERS_LENGTH, Parameters};
use fdt_test::Token;
use i2c_abi::{I2cError, TransferError, i2c, i2c_device};
use i2c_qup::{Bus, Failure, Line, Operation, Resources};
use ipc::{CallError, Protocol};
use kernel_objects::{Session, SessionTransport, Signal};
use mmio::Registers;
use pins_abi::{PinsError, pins};
use platform_abi::{Description, DeviceId};
use runtime::Timeout;
use syscall::SIGNALED;

use crate::{
    composition::{Import, compatibles, controller_provider, pause, request},
    pins::{PINS_REPLY_TIMEOUT, PINS_REQUIREMENT, open_device},
    register_model::{Access, RegisterFile},
    rootkeeper_system::{
        POLL_ATTEMPTS, POLL_INTERVAL, await_revoked, finish, platform_describes, with_description,
        with_drivers,
    },
};

const WINDOW: usize = 0;
const WINDOW_SIZE: usize = 0x600;

const STATE: usize = 0x004;
const IO_MODE: usize = 0x008;
const SW_RESET: usize = 0x00c;
const OPERATIONAL: usize = 0x018;
const ERROR_FLAGS: usize = 0x01c;
const OUTPUT_FIFO: usize = 0x110;
const MX_WRITE_CNT: usize = 0x150;
const INPUT_FIFO: usize = 0x218;
const I2C_CLK_CTL: usize = 0x400;
const I2C_STATUS: usize = 0x404;

const STATE_RESET: u32 = 0;
const STATE_RUN: u32 = 1;
const STATE_PAUSE: u32 = 3;
const STATE_MASK: u32 = 3;
const STATE_VALID: u32 = 1 << 2;
const MASTER_GENERATOR: u32 = 1 << 4;
const NACK: u32 = 1 << 3;
const BUS_ERROR: u32 = 1 << 7;
const OUTPUT_SERVICE: u32 = 1 << 8;
const INPUT_SERVICE: u32 = 1 << 9;
const OUTPUT_DONE: u32 = 1 << 10;
const COUNT_MASK: u32 = 0xffff;

const IO_MODE_FIFO_64: u32 = 1 | 1 << 2 | 1 << 5 | 1 << 7;

const TOUCHSCREEN_ADDRESS: u8 = 0x62;
const XO_HZ: u32 = 19_200_000;

const QUP: &str = "/soc@0/i2c@c175000";
const TOUCHSCREEN: &str = "/soc@0/i2c@c175000/touchscreen@62";
const TEN_BIT: &str = "/soc@0/i2c@c175000/eeprom@50";
const DEFAULT_RATE_DEVICE: &str = "/soc@0/i2c@c176000/sensor@1d";
const FAST_PLUS_QUP: &str = "/soc@0/i2c@c177000";
const FAST_PLUS_DEVICE: &str = "/soc@0/i2c@c177000/codec@1a";
const SLOW_QUP: &str = "/soc@0/i2c@c178000";
const SLOW_DEVICE: &str = "/soc@0/i2c@c178000/sensor@48";

const TREE: &[Token<'static>] = &fdt_test::tree! {
    soc@0 {
        i2c@c175000 {
            compatible = "qcom,i2c-qup-v2.2.1";
            clock-frequency = <400_000>;
            #address-cells = <1>;
            #size-cells = <0>;
            touchscreen@62 {
                compatible = "test,touchscreen";
                reg = <0x62>;
            };
            eeprom@50 {
                compatible = "test,eeprom";
                reg = <0x8000_0050>;
            };
        };
        i2c@c176000 {
            compatible = "qcom,i2c-qup-v2.2.1";
            #address-cells = <1>;
            #size-cells = <0>;
            sensor@1d {
                compatible = "test,sensor";
                reg = <0x1d>;
            };
        };
        i2c@c177000 {
            compatible = "qcom,i2c-qup-v2.2.1";
            clock-frequency = <1_000_000>;
            #address-cells = <1>;
            #size-cells = <0>;
            codec@1a {
                compatible = "test,codec";
                reg = <0x1a>;
            };
        };
        i2c@c178000 {
            compatible = "qcom,i2c-qup-v2.2.1";
            clock-frequency = <10_000>;
            #address-cells = <1>;
            #size-cells = <0>;
            sensor@48 {
                compatible = "test,sensor";
                reg = <0x48>;
            };
        };
    };
};

const BOARD_TOUCHSCREEN: &str = "/soc@0/i2c@c175000/touchscreen@1";
const NT36672A_CHIP_ID: usize = 0x0e;
const NT36672A_IRQ_TYPE: usize = 0x0b;
const NT36672A_RESET_DELAY: Timeout = Timeout::from_millis(100);
const RESET_LINE: &str = "reset";

const NT36672A_IRQ_TYPE_TRIGGERS: [u32; 4] = [1, 2, 8, 4];
const TOUCH_TIMEOUT: Timeout = Timeout::from_secs(10);

const I2C_REQUIREMENT: &str = "i2c";
const I2C_REPLY_TIMEOUT: Timeout = Timeout::from_secs(5);

const BOARD_IMPORTS: &[Import] = &[
    Import {
        declaration: RequirementDeclaration::all(I2C_REQUIREMENT, i2c::Protocol::ID),
        namespace_root: None,
    },
    Import {
        declaration: RequirementDeclaration::all(PINS_REQUIREMENT, pins::Protocol::ID),
        namespace_root: None,
    },
];

#[derive(Clone)]
enum Reply {
    Complete,
    Data(Vec<u8>),
    Status(u32),
    Silent,
    Waiting,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Event {
    Reset,
    State(u32),
    Launch(Vec<u8>),
}

struct Qup {
    registers: RegisterFile,
    output: RefCell<Vec<u32>>,
    input: RefCell<VecDeque<u32>>,
    replies: RefCell<VecDeque<Reply>>,
    events: RefCell<Vec<Event>>,
    raised: Cell<bool>,
    waiting: Cell<bool>,
}

struct Window<'a>(&'a Qup);

struct ModelLine<'a>(&'a Qup);

#[derive(Debug, Clone, PartialEq, Eq)]
enum Call {
    Pins(String),
    Clocks(String, u32),
    Refused(String, Failure<Supply>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Supply {
    Timeout,
}

struct Supplies<'a> {
    qup: &'a Qup,
    secondary: &'a Qup,
    calls: &'a RefCell<Vec<Call>>,
    clocks: Result<(), Supply>,
}

type ModelBus<'a> = Bus<Supplies<'a>>;

impl Qup {
    fn new() -> Self {
        let registers = RegisterFile::new();
        registers.set(WINDOW, IO_MODE, IO_MODE_FIFO_64);
        Self {
            registers,
            output: RefCell::new(Vec::new()),
            input: RefCell::new(VecDeque::new()),
            replies: RefCell::new(VecDeque::new()),
            events: RefCell::new(Vec::new()),
            raised: Cell::new(false),
            waiting: Cell::new(false),
        }
    }

    fn reply(&self, reply: Reply) {
        self.replies.borrow_mut().push_back(reply);
    }

    fn launch(&self) {
        let length = (self.registers.get(WINDOW, MX_WRITE_CNT) & COUNT_MASK) as usize;
        let mut bytes: Vec<u8> = self
            .output
            .take()
            .iter()
            .flat_map(|word| word.to_le_bytes())
            .collect();
        bytes.truncate(length);
        self.events.borrow_mut().push(Event::Launch(bytes));

        match self
            .replies
            .borrow_mut()
            .pop_front()
            .unwrap_or(Reply::Complete)
        {
            Reply::Complete => self.raise(OPERATIONAL, OUTPUT_SERVICE | OUTPUT_DONE),
            Reply::Data(data) => {
                let tagged: Vec<u8> = [0x87, 0].into_iter().chain(data).collect();
                let mut input = self.input.borrow_mut();
                for chunk in tagged.chunks(4) {
                    let mut word = [0; 4];
                    word[..chunk.len()].copy_from_slice(chunk);
                    input.push_back(u32::from_le_bytes(word));
                }
                drop(input);
                self.raise(OPERATIONAL, INPUT_SERVICE);
            }
            Reply::Status(bits) => self.raise(I2C_STATUS, bits),
            Reply::Silent => {}
            Reply::Waiting => self.waiting.set(true),
        }
    }

    fn raise(&self, offset: usize, bits: u32) {
        let value = self.registers.get(WINDOW, offset);
        self.registers.set(WINDOW, offset, value | bits);
        self.raised.set(true);
    }

    fn stores(&self, offset: usize) -> Vec<u32> {
        self.registers
            .take_accesses()
            .into_iter()
            .filter_map(|access| match access {
                Access::Store {
                    offset: stored,
                    value,
                    ..
                } if stored == offset => Some(value),
                _ => None,
            })
            .collect()
    }
}

impl Registers<()> for Window<'_> {
    fn size(&self, (): ()) -> usize {
        WINDOW_SIZE
    }

    fn load(&self, (): (), offset: usize) -> u32 {
        let qup = self.0;
        match offset {
            STATE => {
                qup.registers.load(WINDOW, offset) & STATE_MASK | STATE_VALID | MASTER_GENERATOR
            }
            INPUT_FIFO => {
                qup.registers.load(WINDOW, offset);
                qup.input.borrow_mut().pop_front().unwrap_or(0)
            }
            _ => qup.registers.load(WINDOW, offset),
        }
    }

    fn store(&self, (): (), offset: usize, value: u32) {
        let qup = self.0;
        let previous = qup.registers.get(WINDOW, offset);
        qup.registers.store(WINDOW, offset, value);
        match offset {
            SW_RESET => {
                for register in [STATE, OPERATIONAL, I2C_STATUS, ERROR_FLAGS] {
                    qup.registers.set(WINDOW, register, 0);
                }

                qup.output.borrow_mut().clear();
                qup.input.borrow_mut().clear();
                qup.waiting.set(false);
                qup.events.borrow_mut().push(Event::Reset);
            }
            STATE => {
                qup.events.borrow_mut().push(Event::State(value));
                if value == STATE_RUN && !qup.output.borrow().is_empty() {
                    qup.launch();
                }
            }
            // Флаги сбрасываются записью единиц.
            OPERATIONAL | I2C_STATUS | ERROR_FLAGS => {
                qup.registers.set(WINDOW, offset, previous & !value);
            }
            OUTPUT_FIFO => qup.output.borrow_mut().push(value),
            _ => {}
        }
    }
}

impl Line for ModelLine<'_> {
    async fn wait(&self) -> bool {
        core::future::poll_fn(|_| {
            if self.0.waiting.get() {
                Poll::Pending
            } else {
                Poll::Ready(self.0.raised.take())
            }
        })
        .await
    }

    fn ack(&self) {}

    fn clear(&self) {}
}

impl<'a> Supplies<'a> {
    fn model(&self, controller: &DeviceId) -> &'a Qup {
        if controller.as_str() == "/soc@0/i2c@c176000" {
            self.secondary
        } else {
            self.qup
        }
    }
}

#[allow(clippy::unused_async_trait_impl)]
impl<'a> Resources for Supplies<'a> {
    type Window = Window<'a>;
    type Line = ModelLine<'a>;
    type Error = Supply;

    async fn enable(
        &self,
        _description: &Description<'_>,
        controller: &DeviceId,
        core_hz: u32,
    ) -> Result<Window<'a>, Supply> {
        let path = controller.as_str().to_string();
        let mut calls = self.calls.borrow_mut();
        calls.push(Call::Pins(path.clone()));
        calls.push(Call::Clocks(path, core_hz));
        self.clocks.map(|()| Window(self.model(controller)))
    }

    async fn line(&self, controller: &DeviceId) -> Result<ModelLine<'a>, Supply> {
        Ok(ModelLine(self.model(controller)))
    }

    fn refused(&self, controller: &DeviceId, failure: &Failure<Supply>) {
        self.calls.borrow_mut().push(Call::Refused(
            controller.as_str().to_string(),
            failure.clone(),
        ));
    }
}

fn id(path: &str) -> DeviceId {
    DeviceId::new(path).expect("absolute path")
}

fn with_bus(
    clocks: Result<(), Supply>,
    test: impl FnOnce(&Qup, &Description<'_>, &ModelBus<'_>, &RefCell<Vec<Call>>),
) {
    let blob = fdt_test::build(TREE);
    let description = Description::new(&blob, 0).expect("tree parses");
    let qup = Qup::new();
    let calls = RefCell::new(Vec::new());
    let bus = Bus::new(Supplies {
        qup: &qup,
        secondary: &qup,
        calls: &calls,
        clocks,
    });
    test(&qup, &description, &bus, &calls);
}

fn open(
    description: &Description<'_>,
    bus: &ModelBus<'_>,
    badge: u64,
    path: &str,
) -> Result<(), I2cError> {
    crate::executor::executor().block_on(bus.prepare(description, &id(path)))?;
    bus.claim(badge, id(path));
    Ok(())
}

fn opened_touchscreen(qup: &Qup, description: &Description<'_>, bus: &ModelBus<'_>) {
    open(description, bus, 1, TOUCHSCREEN).expect("touchscreen");
    qup.registers.take_accesses();
    qup.events.take();
}

fn transfer(
    description: &Description<'_>,
    bus: &ModelBus<'_>,
    operation: Operation<'_>,
) -> Result<Vec<u8>, TransferError> {
    let device = bus.device(description, 1).expect("device of the session");
    crate::executor::executor()
        .block_on(device.transfer(operation))
        .map(|received| received.0)
}

#[test_harness::test]
fn the_first_open_prepares_the_controller_once_and_the_device_is_held_until_release() {
    with_bus(Ok(()), |_, description, bus, calls| {
        let first = open(description, bus, 1, TOUCHSCREEN);
        let address = bus.device(description, 1).map(|device| device.address());
        let busy = open(description, bus, 2, TOUCHSCREEN);
        bus.release(1);
        let again = open(description, bus, 2, TOUCHSCREEN);

        test_harness::check_eq!(first, Ok(()));
        test_harness::check_eq!(address, Some(TOUCHSCREEN_ADDRESS));
        test_harness::check_eq!(busy, Err(I2cError::Busy));
        test_harness::check_eq!(again, Ok(()));
        test_harness::check_eq!(
            calls.take(),
            [
                Call::Pins(QUP.to_string()),
                Call::Clocks(QUP.to_string(), XO_HZ)
            ]
        );
    });
}

#[test_harness::test]
fn the_controller_node_and_a_ten_bit_address_are_refused_without_preparation() {
    with_bus(Ok(()), |qup, description, bus, calls| {
        test_harness::check_eq!(open(description, bus, 1, QUP), Err(I2cError::NoDevice));
        test_harness::check_eq!(
            open(description, bus, 1, TEN_BIT),
            Err(I2cError::Unsupported)
        );
        test_harness::check_eq!(calls.take(), []);
        test_harness::check_eq!(qup.registers.take_accesses(), []);
    });
}

#[test_harness::test]
fn a_refused_preparation_is_reported_once_and_open_answers_no_device() {
    with_bus(Err(Supply::Timeout), |qup, description, bus, calls| {
        let first = open(description, bus, 1, TOUCHSCREEN);
        let first_calls = calls.take();
        let second = open(description, bus, 2, TOUCHSCREEN);
        let fast_plus = open(description, bus, 1, FAST_PLUS_DEVICE);
        let slow = open(description, bus, 1, SLOW_DEVICE);

        test_harness::check_eq!(
            [first, second, fast_plus, slow],
            [Err(I2cError::NoDevice); 4]
        );
        test_harness::check_eq!(
            first_calls,
            [
                Call::Pins(QUP.to_string()),
                Call::Clocks(QUP.to_string(), XO_HZ),
                Call::Refused(QUP.to_string(), Failure::Resources(Supply::Timeout)),
            ]
        );
        test_harness::check_eq!(
            calls.take(),
            [
                Call::Refused(FAST_PLUS_QUP.to_string(), Failure::Frequency(1_000_000)),
                Call::Refused(SLOW_QUP.to_string(), Failure::Frequency(10_000)),
            ]
        );
        test_harness::check_eq!(qup.registers.take_accesses(), []);
    });
}

#[test_harness::test]
fn the_first_transfer_writes_the_divider_of_the_described_or_default_frequency() {
    for (device, divider) in [(TOUCHSCREEN, 0xE_031C), (DEFAULT_RATE_DEVICE, 0x35D)] {
        with_bus(Ok(()), |qup, description, bus, _| {
            open(description, bus, 1, device).expect("device");
            qup.registers.take_accesses();

            test_harness::check_eq!(
                transfer(description, bus, Operation::Write(&[0])),
                Ok(vec![])
            );
            test_harness::check_eq!(qup.stores(I2C_CLK_CTL), [divider]);
        });
    }
}

#[test_harness::test]
fn a_write_sends_its_tags_and_data_and_answers_after_completion() {
    with_bus(Ok(()), |qup, description, bus, _| {
        opened_touchscreen(qup, description, bus);

        let written = transfer(description, bus, Operation::Write(&[0xff, 0x01]));

        test_harness::check_eq!(written, Ok(vec![]));
        test_harness::check_eq!(
            qup.events.take(),
            [
                Event::Reset,
                Event::State(STATE_RUN),
                Event::State(STATE_PAUSE),
                Event::State(STATE_RUN),
                Event::Launch(vec![0x81, 0xc4, 0x83, 0x02, 0xff, 0x01]),
                Event::State(STATE_RESET),
            ]
        );
    });
}

#[test_harness::test]
fn write_read_pauses_between_the_write_and_the_repeated_start() {
    with_bus(Ok(()), |qup, description, bus, _| {
        opened_touchscreen(qup, description, bus);
        let parameters: Vec<u8> = (1..=15).collect();
        qup.reply(Reply::Complete);
        qup.reply(Reply::Data(parameters.clone()));

        let read = transfer(
            description,
            bus,
            Operation::WriteRead(&[PARAMETERS_ADDRESS], PARAMETERS_LENGTH),
        );

        test_harness::check_eq!(read, Ok(parameters));
        test_harness::check_eq!(
            qup.events.take(),
            [
                Event::Reset,
                Event::State(STATE_RUN),
                Event::State(STATE_PAUSE),
                Event::State(STATE_RUN),
                Event::Launch(vec![0x81, 0xc4, 0x82, 0x01, 0x78]),
                Event::State(STATE_PAUSE),
                Event::State(STATE_RUN),
                Event::Launch(vec![0x81, 0xc5, 0x87, 0x0f]),
                Event::State(STATE_RESET),
            ]
        );
    });
}

#[test_harness::test]
fn a_transfer_past_the_fifo_or_an_empty_write_is_refused_before_the_registers() {
    with_bus(Ok(()), |qup, description, bus, _| {
        opened_touchscreen(qup, description, bus);

        let too_long = transfer(description, bus, Operation::Read(63));
        let empty = transfer(description, bus, Operation::Write(&[]));
        let refused_accesses = qup.registers.take_accesses();
        qup.reply(Reply::Data((0..62).collect()));
        let fitting = transfer(description, bus, Operation::Read(62));

        test_harness::check_eq!(too_long, Err(TransferError::Length));
        test_harness::check_eq!(empty, Err(TransferError::Length));
        test_harness::check_eq!(refused_accesses, []);
        test_harness::check_eq!(fitting, Ok((0..62).collect()));
    });
}

#[test_harness::test]
fn a_failure_answers_by_its_cause_and_the_next_read_starts_with_a_reset() {
    let failures = [
        (
            Reply::Status(NACK),
            TransferError::NoAck,
            Event::State(STATE_RESET),
        ),
        (
            Reply::Status(BUS_ERROR),
            TransferError::Bus,
            Event::State(STATE_RESET),
        ),
        (Reply::Silent, TransferError::Timeout, Event::Reset),
    ];
    for (reply, error, last) in failures {
        with_bus(Ok(()), |qup, description, bus, _| {
            opened_touchscreen(qup, description, bus);
            qup.reply(reply);

            let refused = transfer(description, bus, Operation::Write(&[0]));
            let refused_last = qup.events.take().last().cloned();
            qup.reply(Reply::Data(vec![1, 2, 3, 4]));
            let read = transfer(description, bus, Operation::Read(4));

            test_harness::check_eq!(refused, Err(error));
            test_harness::check_eq!(refused_last, Some(last));
            test_harness::check_eq!(qup.events.take().first().cloned(), Some(Event::Reset));
            test_harness::check_eq!(read, Ok(vec![1, 2, 3, 4]));
        });
    }
}

#[test_harness::test]
fn i2c_transfers_wait_per_controller_and_cancel_before_the_next_transfer() {
    let blob = fdt_test::build(TREE);
    let description = Description::new(&blob, 0).expect("tree parses");
    let first = Qup::new();
    let second = Qup::new();
    let calls = RefCell::new(Vec::new());
    let bus = Bus::new(Supplies {
        qup: &first,
        secondary: &second,
        calls: &calls,
        clocks: Ok(()),
    });
    open(&description, &bus, 1, TOUCHSCREEN).expect("first controller");
    let device = bus.device(&description, 1).expect("first device");
    first.reply(Reply::Waiting);
    let mut active = alloc::boxed::Box::pin(device.transfer(Operation::Write(&[1])));
    test_harness::check!(
        active
            .as_mut()
            .poll(&mut Context::from_waker(Waker::noop()))
            .is_pending()
    );
    let mut next = alloc::boxed::Box::pin(device.transfer(Operation::Write(&[2])));
    test_harness::check!(
        next.as_mut()
            .poll(&mut Context::from_waker(Waker::noop()))
            .is_pending()
    );
    test_harness::check_eq!(
        first
            .events
            .borrow()
            .iter()
            .filter(|event| matches!(event, Event::Launch(_)))
            .count(),
        1
    );

    open(&description, &bus, 2, DEFAULT_RATE_DEVICE).expect("second controller");
    let independent = bus.device(&description, 2).expect("second device");
    test_harness::check!(
        crate::executor::executor()
            .block_on(independent.transfer(Operation::Write(&[3])))
            .is_ok()
    );
    test_harness::check!(first.waiting.get());
    first.events.take();
    drop(active);
    test_harness::check_eq!(first.events.take(), [Event::Reset]);
    test_harness::check!(crate::executor::executor().block_on(next).is_ok());
    test_harness::check!(!first.waiting.get());
    test_harness::check_eq!(first.events.take().first().cloned(), Some(Event::Reset));
}

fn controller_compatibles(description: &Description<'_>, path: &str) -> Vec<String> {
    let parent = DeviceId::new(path)
        .and_then(|device| description.ancestors(&device).next())
        .map(|ancestor| ancestor.node);
    compatibles(parent)
}

#[test_harness::test]
fn the_board_touchscreen_out_of_reset_answers_its_parameters_and_lends_its_interrupt() {
    if !platform_describes(BOARD_TOUCHSCREEN) {
        test_harness::skip!("{} is not described", BOARD_TOUCHSCREEN);
    }

    let (controller, pins_controller, trigger) = with_description(|description| {
        let touchscreen = id(BOARD_TOUCHSCREEN);
        (
            controller_compatibles(description, BOARD_TOUCHSCREEN),
            crate::pins::controller_compatibles(description, BOARD_TOUCHSCREEN),
            pins_abi::interrupt(description, &touchscreen, &touchscreen, 0)
                .and_then(|record| record.cells.get(1).copied()),
        )
    });

    let composition = with_drivers();
    let importer = composition.importer(BOARD_IMPORTS);
    let unfiltered =
        request::<i2c::Protocol>(importer.handle(), I2C_REQUIREMENT, I2C_REPLY_TIMEOUT).map(drop);

    let pins = controller_provider::<pins::Protocol>(
        importer.handle(),
        PINS_REQUIREMENT,
        &pins_controller,
    )
    .client()
    .with_wait(PINS_REPLY_TIMEOUT);
    let reset = open_device(&pins, id(BOARD_TOUCHSCREEN)).expect("touchscreen pins session");
    let line = reset
        .output(id(BOARD_TOUCHSCREEN), RESET_LINE, 0, false)
        .wait()
        .expect("reset line");

    let reset_irq = reset.irq(line).wait().map(drop);
    let released = reset.active(line).wait();
    pause(NT36672A_RESET_DELAY);

    let bus = controller_provider::<i2c::Protocol>(importer.handle(), I2C_REQUIREMENT, &controller)
        .client()
        .with_wait(I2C_REPLY_TIMEOUT);
    let touchscreen = bus.open(id(BOARD_TOUCHSCREEN)).wait().map(|session| {
        Session::<i2c_device::Protocol>::from_session(session)
            .client()
            .with_wait(I2C_REPLY_TIMEOUT)
    });

    test_harness::check_eq!(unfiltered, Err(CallError::Remote(SessionError::Unbound)));
    match touchscreen {
        Ok(touchscreen) => {
            let read = touchscreen
                .write_read(&[PARAMETERS_ADDRESS], PARAMETERS_LENGTH)
                .wait()
                .map(|buf| buf.0);
            match &read {
                Ok(parameters) => check_touchscreen_parameters(parameters, trigger),
                Err(CallError::Remote(TransferError::NoAck)) => {
                    test_harness::report!("touchscreen answered no ack");
                }
                Err(error) => test_harness::check_eq!(Some(error), None),
            }
        }
        Err(error) => test_harness::check_eq!(Some(error), None),
    }

    let interrupt = reset.interrupt(id(BOARD_TOUCHSCREEN), 0).wait();
    let signal = interrupt
        .as_ref()
        .ok()
        .and_then(|interrupt| reset.irq(*interrupt).wait().ok())
        .map(Signal::from);
    if let Some(signal) = &signal {
        match signal.wait(SIGNALED, TOUCH_TIMEOUT) {
            Ok(_) => {
                test_harness::report!("touchscreen interrupt fired");
            }
            Err(error) => {
                test_harness::report!("touchscreen interrupt quiet: {error:?}");
            }
        }
    }
    let acked = interrupt
        .as_ref()
        .map(|interrupt| reset.ack(*interrupt).wait());

    let asserted = reset
        .set(line, true)
        .wait()
        .and_then(|()| reset.active(line).wait());

    drop(reset);

    let revoked = signal
        .as_ref()
        .is_some_and(|signal| await_revoked(|| signal.wait(SIGNALED, Timeout::from_millis(0))));
    let reissued = reissued_interrupt(&pins);

    test_harness::check_eq!(reset_irq, Err(CallError::Remote(PinsError::NotIssued)));
    test_harness::check!(interrupt.is_ok());
    test_harness::check!(signal.is_some());
    test_harness::check!(trigger.is_some());
    test_harness::check_eq!(acked, Ok(Ok(())));
    test_harness::check_eq!(released, Ok(false));
    test_harness::check_eq!(asserted, Ok(true));
    test_harness::check!(revoked);
    test_harness::check_eq!(reissued, Some(Ok(())));

    finish(composition);
}

fn check_touchscreen_parameters(parameters: &[u8], trigger: Option<u32>) {
    test_harness::check_eq!(parameters.len(), usize::from(PARAMETERS_LENGTH));
    test_harness::report!(
        "touchscreen chip id {:#04x}",
        parameters.get(NT36672A_CHIP_ID).copied().unwrap_or(0)
    );
    match Parameters::parse(parameters) {
        Ok(accepted) => {
            test_harness::report!(
                "touchscreen size {}x{}, {} touches",
                accepted.width(),
                accepted.height(),
                accepted.max_touches()
            );
        }
        Err(refused) => {
            test_harness::report!("touchscreen parameters refused: {refused:?}");
        }
    }
    // Параметр - индекс условия `IRQ_TYPE_*` в таблице `nvt_ts_irq_type` Linux 6.16.
    test_harness::check_eq!(
        parameters
            .get(NT36672A_IRQ_TYPE)
            .and_then(|index| NT36672A_IRQ_TYPE_TRIGGERS.get(usize::from(*index)))
            .copied(),
        trigger
    );
}

fn reissued_interrupt(
    pins: &pins::Client<SessionTransport>,
) -> Option<Result<(), CallError<PinsError>>> {
    let reopened = (0..POLL_ATTEMPTS)
        .find_map(|_| match open_device(pins, id(BOARD_TOUCHSCREEN)) {
            Err(CallError::Remote(PinsError::Busy)) => {
                pause(POLL_INTERVAL);
                None
            }
            opened => Some(opened),
        })?
        .ok()?;

    Some(
        reopened
            .interrupt(id(BOARD_TOUCHSCREEN), 0)
            .wait()
            .and_then(|interrupt| reopened.irq(interrupt).wait())
            .map(drop),
    )
}
