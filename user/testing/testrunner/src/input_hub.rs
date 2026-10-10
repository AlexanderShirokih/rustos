//! Хаб ввода `input-hub`: протоколы `InputInject` и `Input` с ленивым запуском хаба из индекса
//! образа и подключение источников `InputSource` под признаком публикатора.

use alloc::{collections::BTreeSet, sync::Arc, vec, vec::Vec};

use component_abi::RequirementDeclaration;
use component_startup::SessionError;
use input_abi::{
    AxisDecl, AxisList, Binding, Command, DeclareError, DescribeError, DeviceDescriptor, KeyList,
    Kind, NextError, Record, RecordsError, UpdateError, Usage, input, input_inject,
};
use ipc::{CallError, Protocol};
use kernel_objects::{Session, SessionTransport};
use runtime::{JoinHandle, Mutex, PortTransport, Timeout, spawn_owned};

use crate::{
    composition::{Composition, Import, Importer, pause, request},
    log_relay::await_component_lines,
    rootkeeper_system::{PUBLISHER, finish, indexed, registered},
    source_fixture::{SLOW_DEVICE, SOURCE_DEVICE, with_descriptor},
};

const SOURCE: &str = "source";
const INPUT: &str = "input";

const HUB: &str = "input-hub";
const REFUSED_LINE: &str = "source attach failed: Remote(Refused)";

const REQUIREMENTS: &[Import] = &[
    Import {
        declaration: RequirementDeclaration::session(SOURCE, input_inject::Protocol::ID),
        namespace_root: None,
    },
    Import {
        declaration: RequirementDeclaration::session(INPUT, input::Protocol::ID),
        namespace_root: None,
    },
];

const CALL_WAIT: Timeout = Timeout::from_secs(2);

const SOURCE_WAIT: Timeout = Timeout::from_secs(15);

const HUB_SETTLE_WAIT: Timeout = Timeout::from_millis(100);

const INPUT_SESSIONS_MAX: usize = 16;

const SCREEN_WIDTH: i32 = 1080;
const SCREEN_HEIGHT: i32 = 2340;

const CONTACT: u16 = 0;
const X: u16 = 0;
const Y: u16 = 1;
const TOUCH_X: i64 = 540;
const TOUCH_Y: i64 = 1170;
const TOUCH_TIME: u64 = 1000;

const KEYBOARD_PAGE: u16 = 0x07;
const FIRST_KEY_USAGE: u16 = 0x04;

const FLOOD_KEYS: u16 = 63;

const FLOOD_COMMANDS: usize = 2 * 35 + 1;

const WIDE_KEYS: u16 = 252;

const DECLARE_ATTEMPTS: usize = 64;

type Source = input_inject::Client<SessionTransport>;
type Consumer = input::Client<PortTransport>;
type NextAnswer = Result<Vec<Record>, CallError<NextError>>;

struct DescriptorParts {
    binding: Binding,
    keys: KeyList,
    max_contacts: u16,
    contact_axes: AxisList,
    device_axes: AxisList,
}

struct PendingNext {
    thread: JoinHandle,
    answer: Arc<Mutex<Option<NextAnswer>>>,
}

impl DescriptorParts {
    fn descriptor(&self) -> DeviceDescriptor<'_> {
        DeviceDescriptor::new(
            "device",
            self.binding,
            &self.keys,
            &self.device_axes,
            self.max_contacts,
            &self.contact_axes,
        )
        .expect("name fits")
    }
}

impl PendingNext {
    fn start(session: &Session<input::Protocol>) -> Self {
        let port = session.handle().as_raw();
        let answer = Arc::new(Mutex::new(None));
        let slot = Arc::clone(&answer);
        let thread = spawn_owned(move || {
            let consumer = input::Client::new(PortTransport::new(port)).with_wait(CALL_WAIT);
            *slot.lock() = Some(read(&consumer));
            0
        })
        .expect("next thread starts");
        pause(HUB_SETTLE_WAIT);
        Self { thread, answer }
    }

    fn answer(self) -> NextAnswer {
        self.thread.join(CALL_WAIT).expect("next returns");
        self.answer.lock().take().expect("next answer is stored")
    }
}

fn hub() -> (Composition, Importer) {
    let (composition, _) = indexed(&[]);
    let importer = composition.importer(REQUIREMENTS);
    (composition, importer)
}

fn published() -> (Composition, Importer) {
    let (composition, names) = indexed(&[]);
    registered(&composition, &names, PUBLISHER).expect("publisher added");
    await_component_lines(PUBLISHER.name, 1);
    let importer = composition.importer(REQUIREMENTS);
    (composition, importer)
}

fn source(importer: &Importer) -> Source {
    importer
        .session::<input_inject::Protocol>(SOURCE)
        .client()
        .with_wait(CALL_WAIT)
}

pub(crate) fn consumer(session: &Session<input::Protocol>) -> Consumer {
    input::Client::new(PortTransport::new(session.handle().as_raw())).with_wait(CALL_WAIT)
}

fn touchscreen() -> DescriptorParts {
    let mut contact_axes = AxisList::new();
    contact_axes
        .push(AxisDecl::absolute(Usage::X, 0, SCREEN_WIDTH - 1))
        .expect("axis fits");
    contact_axes
        .push(AxisDecl::absolute(Usage::Y, 0, SCREEN_HEIGHT - 1))
        .expect("axis fits");
    DescriptorParts {
        binding: Binding::Screen,
        keys: KeyList::new(),
        max_contacts: 1,
        contact_axes,
        device_axes: AxisList::new(),
    }
}

fn keyboard(count: u16) -> DescriptorParts {
    let mut keys = KeyList::new();
    for id in FIRST_KEY_USAGE..FIRST_KEY_USAGE + count {
        keys.push(Usage::new(KEYBOARD_PAGE, id)).expect("key fits");
    }
    DescriptorParts {
        binding: Binding::None,
        keys,
        max_contacts: 0,
        contact_axes: AxisList::new(),
        device_axes: AxisList::new(),
    }
}

fn declare(source: &Source, parts: &DescriptorParts) -> u16 {
    source.declare(parts.descriptor()).expect("device declared")
}

pub(crate) fn command(
    build: impl FnOnce(&mut Command) -> Result<&mut Command, RecordsError>,
) -> Command {
    let mut command = Command::new();
    build(&mut command).expect("command fits");
    command
}

fn touch() -> Command {
    command(|c| {
        c.down(CONTACT)?
            .contact_axis(CONTACT, X, TOUCH_X)?
            .contact_axis(CONTACT, Y, TOUCH_Y)
    })
}

fn touch_frame(device: u16) -> Vec<Record> {
    vec![
        Record::contact_down(device, CONTACT),
        Record::axis(device, X, CONTACT, TOUCH_X),
        Record::axis(device, Y, CONTACT, TOUCH_Y),
        Record::frame(device, TOUCH_TIME),
    ]
}

fn update(
    source: &Source,
    device: u16,
    time: Option<u64>,
    command: &Command,
) -> Result<(), CallError<UpdateError>> {
    source.update(device, time, command.as_bytes())
}

fn read(consumer: &Consumer) -> NextAnswer {
    consumer.next().map(|records| records.records().collect())
}

fn read_until(consumer: &Consumer, last: Record) -> Vec<Record> {
    let mut records = Vec::new();
    while !records.contains(&last) {
        records.extend(read(consumer).expect("next answers"));
    }
    records
}

fn attached(input: &Session<input::Protocol>, name: &str) -> bool {
    let expected = with_descriptor(name, |descriptor| descriptor.layout())
        .expect("name fits")
        .expect("descriptor is valid");
    let consumer =
        input::Client::new(PortTransport::new(input.handle().as_raw())).with_wait(SOURCE_WAIT);
    while let Ok(records) = read(&consumer) {
        let added = records.iter().any(|record| {
            record.kind == Kind::DeviceAdded
                && consumer.describe(record.device).as_ref() == Ok(&expected)
        });
        if added {
            return true;
        }
    }
    false
}

fn pressed_keys(records: &[Record], device: u16) -> Vec<u16> {
    let mut pressed = BTreeSet::new();
    for record in records {
        match record.kind {
            Kind::Resync => pressed.clear(),
            Kind::Key if record.device == device && record.value == 1 => {
                pressed.insert(record.code);
            }
            Kind::Key if record.device == device => {
                pressed.remove(&record.code);
            }
            _ => {}
        }
    }
    pressed.into_iter().collect()
}

#[test_harness::test]
fn devices_of_one_source_get_distinct_numbers() {
    let (composition, importer) = hub();
    let input = importer.session::<input::Protocol>(INPUT);
    let source = source(&importer);

    let first = declare(&source, &touchscreen());
    let second = declare(&source, &keyboard(1));
    let records = read_until(&consumer(&input), Record::device_added(second));

    test_harness::check!(first != 0 && second != 0 && first != second);
    test_harness::check_eq!(
        records,
        [
            Record::resync(),
            Record::device_added(first),
            Record::device_added(second)
        ]
    );
    drop((source, input));
    finish(composition);
}

#[test_harness::test]
fn a_relative_contact_axis_is_refused_as_malformed() {
    let (composition, importer) = hub();
    let source = source(&importer);
    let mut parts = touchscreen();
    parts.contact_axes = AxisList::new();
    parts
        .contact_axes
        .push(AxisDecl::relative(Usage::X))
        .expect("axis fits");

    let declared = source.declare(parts.descriptor());

    test_harness::check_eq!(declared, Err(CallError::Remote(DeclareError::Malformed)));
    drop(source);
    finish(composition);
}

#[test_harness::test]
fn a_device_beyond_the_start_sequence_budget_is_refused_and_others_keep_working() {
    let (composition, importer) = hub();
    let source = source(&importer);
    let wide = keyboard(WIDE_KEYS);

    let mut declared = Vec::new();
    let mut refusal = None;
    for _ in 0..DECLARE_ATTEMPTS {
        match source.declare(wide.descriptor()) {
            Ok(device) => declared.push(device),
            Err(error) => {
                refusal = Some(error);
                break;
            }
        }
    }
    let pressed = declared
        .first()
        .map(|&device| update(&source, device, None, &command(|c| c.press(0))));

    test_harness::check_eq!(refusal, Some(CallError::Remote(DeclareError::NoSpace)));
    test_harness::check_eq!(pressed, Some(Ok(())));
    drop(source);
    finish(composition);
}

#[test_harness::test]
fn a_touch_reaches_the_consumer_as_a_frame_of_its_device() {
    let (composition, importer) = hub();
    let input = importer.session::<input::Protocol>(INPUT);
    let consumer = consumer(&input);
    let source = source(&importer);
    let device = declare(&source, &touchscreen());
    read_until(&consumer, Record::device_added(device));

    let updated = update(&source, device, Some(TOUCH_TIME), &touch());
    let records = read(&consumer);

    test_harness::check_eq!(updated, Ok(()));
    test_harness::check_eq!(records, Ok(touch_frame(device)));
    drop((source, input));
    finish(composition);
}

#[test_harness::test]
fn a_command_to_a_device_of_another_source_is_refused_without_records() {
    let (composition, importer) = hub();
    let input = importer.session::<input::Protocol>(INPUT);
    let consumer = consumer(&input);
    let owner = source(&importer);
    let stranger = source(&importer);
    let device = declare(&owner, &touchscreen());
    read_until(&consumer, Record::device_added(device));

    let foreign = update(&stranger, device, Some(TOUCH_TIME), &touch());
    let accepted = update(&owner, device, Some(TOUCH_TIME), &touch());
    let records = read(&consumer);

    test_harness::check_eq!(foreign, Err(CallError::Remote(UpdateError::UnknownDevice)));
    test_harness::check_eq!(accepted, Ok(()));
    test_harness::check_eq!(records, Ok(touch_frame(device)));
    drop((owner, stranger, input));
    finish(composition);
}

#[test_harness::test]
fn a_refused_command_leaves_the_device_accepting_commands() {
    let (composition, importer) = hub();
    let source = source(&importer);
    let device = declare(&source, &touchscreen());

    let moved = update(
        &source,
        device,
        None,
        &command(|c| c.contact_axis(CONTACT, X, TOUCH_X)),
    );
    let touched = update(&source, device, None, &touch());

    test_harness::check_eq!(moved, Err(CallError::Remote(UpdateError::Conflict)));
    test_harness::check_eq!(touched, Ok(()));
    drop(source);
    finish(composition);
}

#[test_harness::test]
fn describe_returns_the_declared_descriptor() {
    let (composition, importer) = hub();
    let input = importer.session::<input::Protocol>(INPUT);
    let consumer = consumer(&input);
    let source = source(&importer);
    let parts = touchscreen();
    let device = declare(&source, &parts);
    read_until(&consumer, Record::device_added(device));

    let described = consumer.describe(device);

    let declared = parts.descriptor().layout().expect("descriptor is valid");
    test_harness::check_eq!(described, Ok(declared));
    drop((source, input));
    finish(composition);
}

#[test_harness::test]
fn a_source_closed_mid_gesture_cancels_and_removes_its_device() {
    let (composition, importer) = hub();
    let input = importer.session::<input::Protocol>(INPUT);
    let consumer = consumer(&input);
    let source = source(&importer);
    let device = declare(&source, &touchscreen());
    update(&source, device, Some(TOUCH_TIME), &touch()).expect("touch accepted");
    read_until(&consumer, Record::frame(device, TOUCH_TIME));

    drop(source);
    let records = read_until(&consumer, Record::device_removed(device));
    let described = consumer.describe(device);

    test_harness::check!(matches!(
        records.as_slice(),
        [up, frame, removed]
            if *up == Record::contact_up(device, CONTACT, true)
                && frame.kind == Kind::Frame
                && frame.device == device
                && *removed == Record::device_removed(device)
    ));
    test_harness::check_eq!(
        described,
        Err(CallError::Remote(DescribeError::UnknownDevice))
    );
    drop(input);
    finish(composition);
}

#[test_harness::test]
fn next_on_an_empty_queue_waits_for_the_first_frame() {
    let (composition, importer) = hub();
    let input = importer.session::<input::Protocol>(INPUT);
    let source = source(&importer);
    let device = declare(&source, &touchscreen());
    read_until(&consumer(&input), Record::device_added(device));

    let pending = PendingNext::start(&input);
    let updated = update(&source, device, Some(TOUCH_TIME), &touch());
    let answer = pending.answer();

    test_harness::check_eq!(updated, Ok(()));
    test_harness::check_eq!(answer, Ok(touch_frame(device)));
    drop((source, input));
    finish(composition);
}

#[test_harness::test]
fn a_second_next_while_one_waits_is_busy() {
    let (composition, importer) = hub();
    let input = importer.session::<input::Protocol>(INPUT);
    let consumer = consumer(&input);
    let source = source(&importer);
    let device = declare(&source, &touchscreen());
    read_until(&consumer, Record::device_added(device));

    let pending = PendingNext::start(&input);
    let second = read(&consumer);
    update(&source, device, Some(TOUCH_TIME), &touch()).expect("touch accepted");
    let answer = pending.answer();

    test_harness::check_eq!(second, Err(CallError::Remote(NextError::Busy)));
    test_harness::check_eq!(answer, Ok(touch_frame(device)));
    drop((source, input));
    finish(composition);
}

#[test_harness::test]
fn a_session_opened_mid_gesture_starts_with_the_device_snapshot() {
    let (composition, importer) = hub();
    let source = source(&importer);
    let device = declare(&source, &touchscreen());
    update(&source, device, Some(TOUCH_TIME), &touch()).expect("touch accepted");

    let input = importer.session::<input::Protocol>(INPUT);
    let records = read(&consumer(&input));

    let mut expected = vec![Record::resync(), Record::device_added(device)];
    expected.extend(touch_frame(device));
    test_harness::check_eq!(records, Ok(expected));
    drop((source, input));
    finish(composition);
}

#[test_harness::test]
fn an_unread_queue_overflow_restarts_it_with_the_current_state() {
    let (composition, importer) = hub();
    let input = importer.session::<input::Protocol>(INPUT);
    let source = source(&importer);
    let device = declare(&source, &keyboard(FLOOD_KEYS));
    let press = command(|c| (1..FLOOD_KEYS).try_fold(c.press(0)?, |c, key| c.press(key)));
    let release = command(|c| (1..FLOOD_KEYS).try_fold(c.release(0)?, |c, key| c.release(key)));

    let mut sent = 0;
    for round in 0..FLOOD_COMMANDS {
        let command = if round % 2 == 0 { &press } else { &release };
        update(&source, device, None, command).expect("command accepted");
        sent += usize::from(FLOOD_KEYS) + 1;
    }
    let marker = declare(&source, &keyboard(1));
    let records = read_until(&consumer(&input), Record::device_added(marker));

    test_harness::check!(records.len() < sent);
    test_harness::check!(records.starts_with(&[Record::resync(), Record::device_added(device)]));
    test_harness::check_eq!(
        pressed_keys(&records, device),
        (0..FLOOD_KEYS).collect::<Vec<_>>()
    );
    drop((source, input));
    finish(composition);
}

#[test_harness::test]
fn a_consumer_closed_with_a_waiting_next_leaves_other_sessions_served() {
    let (composition, importer) = hub();
    let leaving = importer.session::<input::Protocol>(INPUT);
    let staying = importer.session::<input::Protocol>(INPUT);
    let source = source(&importer);
    let device = declare(&source, &touchscreen());
    read_until(&consumer(&leaving), Record::device_added(device));
    read_until(&consumer(&staying), Record::device_added(device));

    let pending = PendingNext::start(&leaving);
    drop(leaving);
    let left = pending.answer();
    let updated = update(&source, device, Some(TOUCH_TIME), &touch());
    let records = read(&consumer(&staying));

    test_harness::check!(left.is_err());
    test_harness::check_eq!(updated, Ok(()));
    test_harness::check_eq!(records, Ok(touch_frame(device)));
    drop((source, staying));
    finish(composition);
}

#[test_harness::test]
fn a_session_beyond_the_limit_is_refused_until_one_closes() {
    let (composition, importer) = hub();
    let mut sessions: Vec<_> = (0..INPUT_SESSIONS_MAX)
        .map(|_| importer.session::<input::Protocol>(INPUT))
        .collect();

    let refused = request::<input::Protocol>(importer.handle(), INPUT, CALL_WAIT).map(drop);
    drop(sessions.pop());
    pause(HUB_SETTLE_WAIT);
    let granted = request::<input::Protocol>(importer.handle(), INPUT, CALL_WAIT).map(drop);

    test_harness::check_eq!(refused, Err(CallError::Remote(SessionError::NoSpace)));
    test_harness::check_eq!(granted, Ok(()));
    drop(sessions);
    finish(composition);
}

#[test_harness::test]
fn a_published_source_is_attached_and_a_refusing_one_is_logged() {
    let (composition, importer) = published();
    let input = importer.session::<input::Protocol>(INPUT);

    let added = attached(&input, SOURCE_DEVICE);
    let lines = await_component_lines(HUB, 1);

    test_harness::check!(added);
    test_harness::check!(lines.iter().any(|line| line == REFUSED_LINE));
    drop(input);
    finish(composition);
}

#[test_harness::test]
fn a_source_published_after_the_hub_started_is_attached() {
    let (composition, names) = indexed(&[]);
    let importer = composition.importer(REQUIREMENTS);
    let input = importer.session::<input::Protocol>(INPUT);
    let started = read(&consumer(&input));

    registered(&composition, &names, PUBLISHER).expect("publisher added");
    let added = attached(&input, SOURCE_DEVICE);

    test_harness::check_eq!(started, Ok(vec![Record::resync()]));
    test_harness::check!(added);
    drop(input);
    finish(composition);
}

#[test_harness::test]
fn a_source_accepting_after_the_open_timeout_is_attached_on_retry() {
    let (composition, importer) = published();
    let input = importer.session::<input::Protocol>(INPUT);

    let added = attached(&input, SLOW_DEVICE);

    test_harness::check!(added);
    drop(input);
    finish(composition);
}
