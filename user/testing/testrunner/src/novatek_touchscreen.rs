//! Библиотека тачскрина `driver-novatek-nvt-ts` на синтетических параметрах и отчётах:
//! проверка параметров, раскладка устройства и перевод отчёта касаний.

use alloc::vec::Vec;

use driver_novatek_nvt_ts::{
    AXIS_X, AXIS_Y, Contacts, Device, PARAMETERS_LENGTH, Parameters, ParametersError,
};
use fdt_test::Token;
use input_abi::{AxisDecl, Binding, Command, DeviceNumber, RecordBuf, Tracker, Usage};
use platform_abi::{Description, DeviceId};

const TREE: &[Token<'static>] = &fdt_test::tree! {
    sized {
        compatible = "novatek,nt36672a-ts";
        "touchscreen-size-x" = <1080>;
        "touchscreen-size-y" = <2340>;
    };
    unsized {
        compatible = "novatek,nt36672a-ts";
    };
};

const RELEASED: u8 = 3;
const UPDATED: u8 = 2;
const NEW: u8 = 1;
const INVALID: [u8; 6] = [0xff; 6];

fn parameters(width: u16, height: u16, touches: u8, chip: u8) -> Vec<u8> {
    let mut bytes = alloc::vec![0; usize::from(PARAMETERS_LENGTH)];
    let [width_high, width_low] = width.to_be_bytes();
    let [height_high, height_low] = height.to_be_bytes();
    for (offset, value) in [
        (0x04, width_high),
        (0x05, width_low),
        (0x06, height_high),
        (0x07, height_low),
        (0x09, touches),
        (0x0b, 1),
        (0x0c, 0x01),
        (0x0e, chip),
    ] {
        if let Some(byte) = bytes.get_mut(offset) {
            *byte = value;
        }
    }
    bytes
}

fn ten_touches() -> Parameters {
    Parameters::parse(&parameters(1080, 2340, 10, 0x08)).expect("parameters")
}

fn device(node: &str, parameters: Parameters) -> Device {
    let blob = fdt_test::build(TREE);
    let description = Description::new(&blob, 0).expect("tree parses");
    let node = description
        .node(&DeviceId::new(node).expect("absolute path"))
        .expect("node described");
    Device::new(&node, parameters).expect("device layout")
}

fn record(slot: u8, state: u8, x: u16, y: u16) -> [u8; 6] {
    let byte = |value: u16| u8::try_from(value).unwrap_or_default();
    [
        slot << 3 | state,
        byte(x >> 4),
        byte(y >> 4),
        byte((x & 0x0f) << 4 | (y & 0x0f)),
        0,
        0,
    ]
}

fn report(records: &[[u8; 6]]) -> Vec<u8> {
    let mut bytes: Vec<u8> = records.iter().flatten().copied().collect();
    bytes.resize(6 * 10, 0xff);
    bytes
}

fn bytes(command: &Command) -> Vec<u8> {
    command.as_bytes().to_vec()
}

fn translated(contacts: &mut Contacts, records: &[[u8; 6]]) -> Vec<u8> {
    contacts
        .translate(&report(records))
        .map(|command| command.as_bytes().to_vec())
        .expect("command fits")
}

fn touch(command: &mut Command, contact: u16, x: i64, y: i64) -> &mut Command {
    command
        .contact_axis(contact, AXIS_X, x)
        .and_then(|command| command.contact_axis(contact, AXIS_Y, y))
        .expect("command fits")
}

#[test_harness::test]
fn parameters_outside_the_probe_limits_or_of_another_chip_are_refused() {
    let refused = [
        parameters(1080, 2340, 10, 0x05),
        parameters(1080, 2340, 11, 0x08),
        parameters(1080, 2340, 0, 0x08),
        parameters(4097, 2340, 10, 0x08),
        parameters(1080, 4096, 10, 0x08),
    ]
    .map(|bytes| Parameters::parse(&bytes));
    let mut irq_type = parameters(1080, 2340, 10, 0x08);
    let mut wake_type = irq_type.clone();
    if let (Some(irq), Some(wake)) = (irq_type.get_mut(0x0b), wake_type.get_mut(0x0c)) {
        (*irq, *wake) = (4, 0x05);
    }
    let short = parameters(1080, 2340, 10, 0x08);

    test_harness::check_eq!(
        refused,
        [
            Err(ParametersError::Chip(0x05)),
            Err(ParametersError::Touches(11)),
            Err(ParametersError::Touches(0)),
            Err(ParametersError::Width(4097)),
            Err(ParametersError::Height(4096)),
        ]
    );
    test_harness::check_eq!(
        Parameters::parse(&irq_type),
        Err(ParametersError::IrqType(4))
    );
    test_harness::check_eq!(
        Parameters::parse(&wake_type),
        Err(ParametersError::WakeType(0x05))
    );
    test_harness::check_eq!(
        Parameters::parse(short.get(..14).unwrap_or_default()),
        Err(ParametersError::Length(14))
    );
    let accepted = ten_touches();
    test_harness::check_eq!(
        (accepted.width(), accepted.height(), accepted.max_touches()),
        (1080, 2340, 10)
    );
}

#[test_harness::test]
fn the_axes_span_the_node_size_or_the_chip_size_without_it() {
    let small = Parameters::parse(&parameters(720, 1440, 5, 0x08)).expect("parameters");
    let sized = device("/sized", ten_touches());
    let bare = device("/unsized", small);

    let layout = sized.descriptor().map(|descriptor| descriptor.layout());

    test_harness::check_eq!(sized.max_contacts(), 10);
    test_harness::check_eq!((sized.max_x(), sized.max_y()), (1079, 2339));
    test_harness::check_eq!(bare.max_contacts(), 5);
    test_harness::check_eq!((bare.max_x(), bare.max_y()), (719, 1439));
    let Some(Ok(layout)) = layout else {
        test_harness::check!(false);
        return;
    };
    test_harness::check_eq!(layout.binding(), Binding::Screen);
    test_harness::check_eq!(
        [layout.contact_axis(AXIS_X), layout.contact_axis(AXIS_Y)],
        [
            Some(AxisDecl::absolute(Usage::X, 0, 1079)),
            Some(AxisDecl::absolute(Usage::Y, 0, 2339)),
        ]
    );
}

#[test_harness::test]
fn a_record_names_its_contact_and_coordinates_clamped_to_the_axes() {
    let mut contacts = Contacts::new(&device("/sized", ten_touches()));

    let first = contacts.translate(&report(&[[0x09, 0x21, 0x43, 0x65, 0, 0]]));
    let clamped = translated(&mut contacts, &[record(1, UPDATED, 1100, 3000)]);

    let mut expected = Command::new();
    expected.down(0).expect("command fits");
    touch(&mut expected, 0, 0x216, 0x435);
    let mut moved = Command::new();
    touch(&mut moved, 0, 1079, 2339);

    test_harness::check_eq!(
        first.map(|command| command.as_bytes().to_vec()),
        Ok(bytes(&expected))
    );
    test_harness::check_eq!(clamped, bytes(&moved));
}

#[test_harness::test]
fn invalid_records_slots_out_of_range_and_unknown_states_change_no_contact() {
    let mut contacts = Contacts::new(&device("/sized", ten_touches()));

    let empty = translated(&mut contacts, &[INVALID, INVALID]);
    let skipped = translated(
        &mut contacts,
        &[
            record(0, NEW, 10, 10),
            record(11, NEW, 10, 10),
            record(2, 5, 10, 10),
            record(3, RELEASED, 10, 10),
        ],
    );

    test_harness::check!(empty.is_empty());
    test_harness::check!(skipped.is_empty());
}

#[test_harness::test]
fn a_touch_moves_and_ends_with_its_release() {
    let mut contacts = Contacts::new(&device("/sized", ten_touches()));

    let down = translated(&mut contacts, &[record(1, NEW, 100, 200)]);
    let moved = translated(
        &mut contacts,
        &[record(1, UPDATED, 110, 210), record(1, RELEASED, 0, 0)],
    );
    let up = translated(&mut contacts, &[record(1, RELEASED, 110, 210)]);

    let mut expected_down = Command::new();
    expected_down.down(0).expect("command fits");
    touch(&mut expected_down, 0, 100, 200);
    let mut expected_moved = Command::new();
    touch(&mut expected_moved, 0, 110, 210);
    let mut expected_up = Command::new();
    expected_up.up(0).expect("command fits");

    test_harness::check_eq!(down, bytes(&expected_down));
    test_harness::check_eq!(moved, bytes(&expected_moved));
    test_harness::check_eq!(up, bytes(&expected_up));
}

#[test_harness::test]
fn a_held_contact_missing_from_the_report_ends_its_touch() {
    let mut contacts = Contacts::new(&device("/sized", ten_touches()));

    translated(
        &mut contacts,
        &[record(1, NEW, 100, 200), record(2, NEW, 300, 400)],
    );
    let dropped = translated(&mut contacts, &[record(1, UPDATED, 110, 200)]);

    let mut expected = Command::new();
    expected.contact_axis(0, AXIS_X, 110).expect("command fits");
    expected.up(1).expect("command fits");

    test_harness::check_eq!(dropped, bytes(&expected));
}

#[test_harness::test]
fn the_hub_accepts_every_command_of_a_touch_sequence() {
    let device = device("/sized", ten_touches());
    let layout = device
        .descriptor()
        .map(|descriptor| descriptor.layout())
        .expect("descriptor")
        .expect("layout");
    let mut tracker = Tracker::new(DeviceNumber::new(1).expect("number"), layout);
    let mut contacts = Contacts::new(&device);
    let reports: [&[[u8; 6]]; 6] = [
        &[record(1, NEW, 100, 200), record(2, NEW, 300, 400)],
        &[record(1, UPDATED, 4000, 4000)],
        &[record(1, UPDATED, 110, 210), record(2, NEW, 310, 410)],
        &[record(2, UPDATED, 320, 420), record(2, RELEASED, 0, 0)],
        &[record(1, RELEASED, 0, 0), record(3, RELEASED, 0, 0)],
        &[INVALID],
    ];

    let mut applied = Vec::new();
    for (time, records) in (1u64..).zip(reports) {
        let command = translated(&mut contacts, records);
        if command.is_empty() {
            continue;
        }
        let mut out = RecordBuf::new();
        applied.push(tracker.apply(&command, Some(time), time, &mut out));
    }

    test_harness::check_eq!(applied.len(), 5);
    test_harness::check!(applied.iter().all(Result::is_ok));
}

#[test_harness::test]
fn a_still_touch_gives_no_command() {
    let mut contacts = Contacts::new(&device("/sized", ten_touches()));

    translated(&mut contacts, &[record(1, NEW, 100, 200)]);
    let still = translated(&mut contacts, &[record(1, UPDATED, 100, 200)]);

    test_harness::check!(still.is_empty());
}
