#![allow(clippy::expect_used)]

use input_abi::{
    AxisDecl, AxisList, Binding, COMMAND_RECORDS_MAX, Command, CommandError, DeviceDescriptor,
    DeviceLayout, KeyList, NO_CONTACT, Record, RecordBuf, RecordsError, Tracker, Usage,
};

const DEVICE: u16 = 7;
const SCREEN_MAX: i32 = 1079;

const X: u16 = 0;
const Y: u16 = 1;

const POWER: u16 = 0;
const VOLUME_UP: u16 = 1;
const ENTER: u16 = 2;
const UNDECLARED_KEY: u16 = 3;

const UNDECLARED_AXIS: u16 = 7;

fn layout(binding: Binding, keys: &[Usage], device: &[AxisDecl], contacts: u16) -> DeviceLayout {
    let mut key_list = KeyList::new();
    for &key in keys {
        key_list.push(key).expect("room");
    }
    let mut device_axes = AxisList::new();
    for &decl in device {
        device_axes.push(decl).expect("room");
    }
    let mut contact_axes = AxisList::new();
    if contacts > 0 {
        contact_axes
            .push(AxisDecl::absolute(Usage::X, 0, SCREEN_MAX))
            .expect("room");
        contact_axes
            .push(AxisDecl::absolute(Usage::Y, 0, SCREEN_MAX))
            .expect("room");
    }
    let descriptor = DeviceDescriptor::new(
        "test",
        binding,
        &key_list,
        &device_axes,
        contacts,
        &contact_axes,
    )
    .expect("short name");
    descriptor.layout().expect("valid")
}

fn tracker(binding: Binding, keys: &[Usage], device: &[AxisDecl], contacts: u16) -> Tracker {
    Tracker::new(DEVICE, layout(binding, keys, device, contacts))
}

fn touch() -> Tracker {
    let keys = [Usage::POWER, Usage::VOLUME_UP, Usage::ENTER];
    tracker(Binding::Screen, &keys, &[], 2)
}

fn mouse() -> Tracker {
    let axes = [AxisDecl::relative(Usage::X), AxisDecl::relative(Usage::Y)];
    tracker(Binding::None, &[Usage::PRIMARY], &axes, 0)
}

fn tablet() -> Tracker {
    let axes = [
        AxisDecl::absolute(Usage::X, 0, SCREEN_MAX),
        AxisDecl::absolute(Usage::Y, 0, SCREEN_MAX),
    ];
    tracker(Binding::Screen, &[], &axes, 0)
}

fn command(build: impl FnOnce(&mut Command) -> Result<&mut Command, RecordsError>) -> Vec<u8> {
    let mut command = Command::new();
    build(&mut command).expect("room");
    command.as_bytes().to_vec()
}

fn raw(records: &[Record]) -> Vec<u8> {
    records.iter().flat_map(Record::to_bytes).collect()
}

fn touch_at(contact: u16, x: i64, y: i64) -> Vec<u8> {
    command(|c| {
        c.down(contact)?
            .contact_axis(contact, X, x)?
            .contact_axis(contact, Y, y)
    })
}

fn apply(tracker: &mut Tracker, command: &[u8], time: Option<u64>, now: u64) -> Vec<Record> {
    let mut out = RecordBuf::new();
    tracker
        .apply(command, time, now, &mut out)
        .expect("accepted");
    out.records().collect()
}

fn snapshot(tracker: &Tracker) -> Vec<Record> {
    let mut out = RecordBuf::new();
    tracker.snapshot(&mut out);
    out.records().collect()
}

fn refuse(tracker: &mut Tracker, command: &[u8], time: Option<u64>) -> CommandError {
    let before = snapshot(tracker);
    let mut out = RecordBuf::new();
    out.push(Record::resync()).expect("room");

    let error = tracker
        .apply(command, time, 0, &mut out)
        .expect_err("refused");

    assert_eq!(snapshot(tracker), before);
    assert_eq!(out.records().collect::<Vec<_>>(), [Record::resync()]);
    error
}

fn axis(contact: u16, axis: u16, value: i64) -> Record {
    Record::axis(DEVICE, axis, contact, value)
}

#[test]
fn two_contacts_of_one_command_come_in_one_frame() {
    let mut touch = touch();
    let both = command(|c| {
        c.down(0)?
            .contact_axis(0, X, 1)?
            .contact_axis(0, Y, 2)?
            .down(1)?
            .contact_axis(1, X, 3)?
            .contact_axis(1, Y, 4)
    });

    let frame = apply(&mut touch, both.as_slice(), Some(10), 0);

    assert_eq!(
        frame,
        [
            Record::contact_down(DEVICE, 0),
            axis(0, X, 1),
            axis(0, Y, 2),
            Record::contact_down(DEVICE, 1),
            axis(1, X, 3),
            axis(1, Y, 4),
            Record::frame(DEVICE, 10),
        ]
    );
}

#[test]
fn a_contact_outside_the_command_keeps_its_axes() {
    let mut touch = touch();
    apply(&mut touch, touch_at(0, 5, 6).as_slice(), None, 1);
    apply(&mut touch, touch_at(1, 7, 8).as_slice(), None, 2);

    let frame = apply(
        &mut touch,
        command(|c| c.contact_axis(1, X, 9)).as_slice(),
        None,
        3,
    );

    assert_eq!(frame, [axis(1, X, 9), Record::frame(DEVICE, 3)]);
    assert_eq!(
        snapshot(&touch)[..3],
        [
            Record::contact_down(DEVICE, 0),
            axis(0, X, 5),
            axis(0, Y, 6),
        ]
    );
}

#[test]
fn a_relative_axis_passes_its_delta_without_state() {
    let mut mouse = mouse();

    let frame = apply(&mut mouse, command(|c| c.axis(X, 5)).as_slice(), None, 4);

    assert_eq!(frame, [axis(NO_CONTACT, X, 5), Record::frame(DEVICE, 4)]);
    assert_eq!(snapshot(&mouse), [Record::frame(DEVICE, 4)]);
}

#[test]
fn a_released_contact_number_starts_a_new_touch_without_motion() {
    let mut touch = touch();
    apply(&mut touch, touch_at(0, 5, 6).as_slice(), None, 1);

    let up = apply(&mut touch, command(|c| c.up(0)).as_slice(), None, 2);
    let down = apply(&mut touch, touch_at(0, 100, 200).as_slice(), None, 3);

    assert_eq!(
        up,
        [
            Record::contact_up(DEVICE, 0, false),
            Record::frame(DEVICE, 2)
        ]
    );
    assert_eq!(
        down,
        [
            Record::contact_down(DEVICE, 0),
            axis(0, X, 100),
            axis(0, Y, 200),
            Record::frame(DEVICE, 3),
        ]
    );
}

#[test]
fn a_key_and_a_touch_of_one_command_make_one_frame() {
    let mut touch = touch();
    let both = command(|c| {
        c.press(POWER)?
            .down(0)?
            .contact_axis(0, X, 1)?
            .contact_axis(0, Y, 2)
    });

    let frame = apply(&mut touch, both.as_slice(), None, 5);

    assert_eq!(
        frame,
        [
            Record::key(DEVICE, POWER, true),
            Record::contact_down(DEVICE, 0),
            axis(0, X, 1),
            axis(0, Y, 2),
            Record::frame(DEVICE, 5),
        ]
    );
}

#[test]
fn a_key_record_carries_the_index_of_the_key_in_the_list() {
    let layout = layout(Binding::None, &[Usage::PRIMARY, Usage::SECONDARY], &[], 0);
    assert_eq!(layout.key(1), Some(Usage::SECONDARY));
    let mut pointer = Tracker::new(DEVICE, layout);

    let frame = apply(&mut pointer, command(|c| c.press(1)).as_slice(), Some(1), 0);

    assert_eq!(
        frame,
        [Record::key(DEVICE, 1, true), Record::frame(DEVICE, 1)]
    );
}

#[test]
fn a_refusal_mid_command_leaves_no_trace() {
    let mut touch = touch();
    let partial = command(|c| {
        c.down(0)?
            .contact_axis(0, X, 1)?
            .contact_axis(0, Y, 2)?
            .contact_axis(1, X, 3)
    });

    assert_eq!(
        refuse(&mut touch, partial.as_slice(), None),
        CommandError::Conflict
    );
    assert_eq!(snapshot(&touch), [Record::frame(DEVICE, 0)]);
}

#[test]
fn contact_refusals() {
    let mut touch = touch();
    apply(&mut touch, touch_at(0, 1, 1).as_slice(), None, 1);

    let cases = [
        (touch_at(2, 1, 1), CommandError::Undeclared),
        (touch_at(0, 1, 1), CommandError::Conflict),
        (command(|c| c.up(1)), CommandError::Conflict),
        (
            command(|c| c.contact_axis(1, X, 1)?.down(1)),
            CommandError::Conflict,
        ),
        (command(|c| c.up(0)?.down(0)), CommandError::Conflict),
        (
            command(|c| c.down(1)?.contact_axis(1, X, 1)),
            CommandError::Conflict,
        ),
    ];

    for (command, expected) in cases {
        assert_eq!(refuse(&mut touch, command.as_slice(), None), expected);
    }
}

#[test]
fn axis_and_key_refusals() {
    let mut touch = touch();
    apply(
        &mut touch,
        command(|c| c.press(VOLUME_UP)).as_slice(),
        None,
        1,
    );
    let mut mouse = mouse();

    let touch_cases = [
        (
            command(|c| c.down(0)?.contact_axis(0, UNDECLARED_AXIS, 1)),
            CommandError::Undeclared,
        ),
        (
            touch_at(0, i64::from(SCREEN_MAX) + 1, 0),
            CommandError::Undeclared,
        ),
        (
            command(|c| c.down(0)?.contact_axis(0, X, 1)?.contact_axis(0, X, 2)),
            CommandError::Conflict,
        ),
        (
            command(|c| c.press(UNDECLARED_KEY)),
            CommandError::Undeclared,
        ),
        (command(|c| c.press(VOLUME_UP)), CommandError::Conflict),
        (
            command(|c| c.press(POWER)?.release(POWER)),
            CommandError::Conflict,
        ),
    ];
    for (command, expected) in touch_cases {
        assert_eq!(refuse(&mut touch, command.as_slice(), None), expected);
    }

    let mut two = Record::key(0, POWER, true);
    two.value = 2;
    assert_eq!(
        refuse(&mut touch, &raw(&[two]), None),
        CommandError::Malformed
    );

    let device_cases = [
        (
            command(|c| c.axis(UNDECLARED_AXIS, 1)),
            CommandError::Undeclared,
        ),
        (
            command(|c| c.axis(X, 1)?.axis(X, 2)),
            CommandError::Conflict,
        ),
    ];
    for (command, expected) in device_cases {
        assert_eq!(refuse(&mut mouse, command.as_slice(), None), expected);
    }
}

#[test]
fn a_key_outside_the_list_is_refused() {
    let mut power = tracker(Binding::None, &[Usage::POWER], &[], 0);

    assert_eq!(
        refuse(&mut power, command(|c| c.press(1)).as_slice(), None),
        CommandError::Undeclared
    );
}

#[test]
fn time_and_encoding_refusals() {
    let mut touch = touch();
    apply(
        &mut touch,
        command(|c| c.press(POWER)).as_slice(),
        Some(100),
        0,
    );
    let release = command(|c| c.release(POWER));

    let mut foreign = Record::key(0, POWER, false);
    foreign.device = 1;
    let mut flagged = Record::key(0, POWER, false);
    flagged.flags = 1;
    let mut misaligned = release.as_slice().to_vec();
    misaligned.push(0);
    let long = raw(&vec![Record::key(0, POWER, false); COMMAND_RECORDS_MAX + 1]);

    let cases = [
        (
            release.as_slice().to_vec(),
            Some(99),
            CommandError::Conflict,
        ),
        (Vec::new(), None, CommandError::Malformed),
        (long, None, CommandError::Malformed),
        (raw(&[Record::frame(0, 200)]), None, CommandError::Malformed),
        (raw(&[foreign]), None, CommandError::Malformed),
        (raw(&[flagged]), None, CommandError::Malformed),
        (misaligned, None, CommandError::Malformed),
    ];
    for (command, time, expected) in cases {
        assert_eq!(refuse(&mut touch, &command, time), expected);
    }
}

#[test]
fn a_command_without_time_after_a_future_frame_takes_that_frame_time() {
    let mut touch = touch();
    apply(
        &mut touch,
        command(|c| c.press(POWER)).as_slice(),
        Some(500),
        0,
    );

    let frame = apply(
        &mut touch,
        command(|c| c.release(POWER)).as_slice(),
        None,
        20,
    );

    assert_eq!(frame.last(), Some(&Record::frame(DEVICE, 500)));
}

#[test]
fn a_snapshot_mid_gesture_holds_keys_and_contacts() {
    let mut touch = touch();
    let gesture = command(|c| {
        c.press(VOLUME_UP)?
            .down(0)?
            .contact_axis(0, Y, 8)?
            .contact_axis(0, X, 9)
    });
    apply(&mut touch, gesture.as_slice(), Some(30), 0);

    assert_eq!(
        snapshot(&touch),
        [
            Record::key(DEVICE, VOLUME_UP, true),
            Record::contact_down(DEVICE, 0),
            axis(0, X, 9),
            axis(0, Y, 8),
            Record::frame(DEVICE, 30),
        ]
    );
}

#[test]
fn a_tablet_snapshot_before_first_motion_is_only_a_frame() {
    assert_eq!(snapshot(&tablet()), [Record::frame(DEVICE, 0)]);
}

#[test]
fn cancel_releases_contacts_and_keys_and_keeps_device_axes() {
    let mut touch = touch();
    let gesture = command(|c| {
        c.press(ENTER)?
            .down(0)?
            .contact_axis(0, X, 1)?
            .contact_axis(0, Y, 2)
    });
    apply(&mut touch, gesture.as_slice(), Some(40), 0);
    let mut tablet = tablet();
    apply(
        &mut tablet,
        command(|c| c.axis(X, 3)?.axis(Y, 4)).as_slice(),
        None,
        1,
    );
    let mut out = RecordBuf::new();

    touch.cancel(10, &mut out);

    assert_eq!(
        out.records().collect::<Vec<_>>(),
        [
            Record::contact_up(DEVICE, 0, true),
            Record::key(DEVICE, ENTER, false),
            Record::frame(DEVICE, 40),
        ]
    );
    assert_eq!(snapshot(&touch), [Record::frame(DEVICE, 40)]);

    tablet.cancel(50, &mut out);

    assert!(out.is_empty());
    assert_eq!(
        snapshot(&tablet),
        [
            axis(NO_CONTACT, X, 3),
            axis(NO_CONTACT, Y, 4),
            Record::frame(DEVICE, 1),
        ]
    );
}
