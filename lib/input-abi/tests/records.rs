#![allow(clippy::expect_used)]

use input_abi::{
    CANCEL, Command, DeviceNumber, Kind, NO_CONTACT, RECORD_SIZE, Record, RecordBuf, Records,
    RecordsError,
};
use ipc::{WireValue, wire::IpcError};

fn device(raw: u16) -> DeviceNumber {
    DeviceNumber::new(raw).expect("nonzero")
}

fn read(bytes: &[u8]) -> Vec<Record> {
    Records::read(bytes).expect("aligned").collect()
}

#[test]
fn a_record_of_every_kind_reads_back_as_written() {
    let written = [
        Record::key(Some(device(1)), 3, true),
        Record::axis(Some(device(2)), 0, NO_CONTACT, -5),
        Record::axis(Some(device(3)), 1, 7, i64::MAX),
        Record::contact_down(Some(device(4)), 15),
        Record::contact_up(Some(device(5)), 9, true),
        Record::frame(Some(device(6)), u64::MAX),
        Record::device_added(device(7)),
        Record::device_removed(device(8)),
        Record::resync(),
    ];
    let mut buf = RecordBuf::new();
    for record in written {
        buf.push(record).expect("room");
    }

    assert_eq!(buf.as_bytes().len(), written.len() * RECORD_SIZE);
    assert_eq!(read(buf.as_bytes()), written);
    assert_eq!(written[4].flags, CANCEL);
    assert_eq!(written[5].nanos(), u64::MAX);
}

#[test]
fn device_removed_reads_as_the_leaving_device() {
    let bytes = Record::device_removed(device(3)).to_bytes();

    let records = read(&bytes);

    assert_eq!(records.len(), 1);
    assert_eq!(records[0].kind, Kind::DeviceRemoved);
    assert_eq!(records[0].device, Some(device(3)));
}

#[test]
fn an_unknown_kind_between_known_records_reads_as_skippable() {
    let mut unknown = Record::frame(Some(device(1)), 0).to_bytes();
    unknown[0] = 0xEE;
    let bytes = [
        Record::key(Some(device(1)), 2, true).to_bytes(),
        unknown,
        Record::frame(Some(device(1)), 10).to_bytes(),
    ]
    .concat();

    let kinds: Vec<Kind> = read(&bytes).iter().map(|record| record.kind).collect();

    assert_eq!(kinds, [Kind::Key, Kind::Unknown(0xEE), Kind::Frame]);
}

#[test]
fn a_sequence_not_a_multiple_of_the_record_size_is_refused() {
    let bytes = [0; RECORD_SIZE + 1];

    assert!(matches!(
        Records::read(&bytes),
        Err(RecordsError::Misaligned)
    ));
}

#[test]
fn a_built_command_reads_as_the_same_records_in_order() {
    let mut command = Command::new();
    command
        .press(0)
        .and_then(|c| c.release(2))
        .and_then(|c| c.axis(0, -3))
        .and_then(|c| c.down(2))
        .and_then(|c| c.contact_axis(2, 1, 40))
        .and_then(|c| c.up(1))
        .and_then(|c| c.cancel(0))
        .expect("room");

    assert_eq!(
        read(command.as_bytes()),
        [
            Record::key(None, 0, true),
            Record::key(None, 2, false),
            Record::axis(None, 0, NO_CONTACT, -3),
            Record::contact_down(None, 2),
            Record::axis(None, 1, 2, 40),
            Record::contact_up(None, 1, false),
            Record::contact_up(None, 0, true),
        ]
    );
}

#[test]
fn a_record_field_of_misaligned_length_is_refused() {
    let bytes = [0; RECORD_SIZE + 1];

    let read = <RecordBuf as WireValue<'_, ()>>::from_field(&bytes, &mut ());

    assert_eq!(read.err(), Some(IpcError::InvalidValue));
}
