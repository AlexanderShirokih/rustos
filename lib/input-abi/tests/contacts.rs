#![allow(clippy::expect_used)]

use input_abi::{
    AxisDecl, AxisList, Binding, Command, ContactTable, DeviceDescriptor, DeviceNumber, KeyList,
    RecordBuf, Tracker, Usage,
};

const DEVICE: DeviceNumber = DeviceNumber::new(3).expect("nonzero");
const CONTACTS: u16 = 2;
const X: u16 = 0;
const Y: u16 = 1;

struct Screen {
    table: ContactTable,
    tracker: Tracker,
    time: u64,
}

impl Screen {
    fn new() -> Self {
        let mut axes = AxisList::new();
        for usage in [Usage::X, Usage::Y] {
            axes.push(AxisDecl::absolute(usage, 0, 1000)).expect("room");
        }
        let (keys, device_axes) = (KeyList::new(), AxisList::new());
        let layout = DeviceDescriptor::new(
            "screen",
            Binding::Screen,
            &keys,
            &device_axes,
            CONTACTS,
            &axes,
        )
        .expect("short name")
        .layout()
        .expect("valid");
        Self {
            table: ContactTable::new(CONTACTS, 2),
            tracker: Tracker::new(DEVICE, layout),
            time: 0,
        }
    }

    fn position(&mut self, contact: u16, x: i32, y: i32) {
        self.table.axis(contact, X, x);
        self.table.axis(contact, Y, y);
    }

    fn finish(&mut self) -> Vec<Vec<u8>> {
        let mut command = Command::new();
        let restarted = self.table.finish(&mut command).expect("command fits");
        self.applied([Some(command), restarted])
    }

    fn cancel(&mut self) -> Vec<Vec<u8>> {
        let mut command = Command::new();
        self.table.cancel(&mut command).expect("command fits");
        self.applied([Some(command), None])
    }

    fn applied(&mut self, commands: [Option<Command>; 2]) -> Vec<Vec<u8>> {
        let mut sent = Vec::new();
        for command in commands.into_iter().flatten() {
            if command.as_bytes().is_empty() {
                continue;
            }
            self.time += 1;
            let mut out = RecordBuf::new();
            self.tracker
                .apply(command.as_bytes(), Some(self.time), self.time, &mut out)
                .expect("tracker accepts the command");
            sent.push(command.as_bytes().to_vec());
        }
        sent
    }
}

fn bytes(build: impl FnOnce(&mut Command)) -> Vec<u8> {
    let mut command = Command::new();
    build(&mut command);
    command.as_bytes().to_vec()
}

fn down(command: &mut Command, contact: u16, x: i64, y: i64) {
    command.down(contact).expect("room");
    command.contact_axis(contact, X, x).expect("room");
    command.contact_axis(contact, Y, y).expect("room");
}

#[test]
fn a_touch_begins_with_every_axis_and_then_sends_only_changed_axes() {
    let mut screen = Screen::new();

    screen.position(0, 10, 20);
    screen.table.touch(0);
    let began = screen.finish();
    screen.position(0, 10, 20);
    screen.table.touch(0);
    let still = screen.finish();
    screen.position(0, 15, 20);
    screen.table.touch(0);
    let moved = screen.finish();

    assert_eq!(began, [bytes(|command| down(command, 0, 10, 20))]);
    assert!(still.is_empty());
    assert_eq!(
        moved,
        [bytes(|command| {
            command.contact_axis(0, X, 15).expect("room");
        })]
    );
}

#[test]
fn a_touch_waits_for_every_axis_across_frames() {
    let mut screen = Screen::new();

    screen.table.axis(1, X, 30);
    screen.table.touch(1);
    let waiting = screen.finish();
    screen.table.axis(1, Y, 40);
    let began = screen.finish();

    assert!(waiting.is_empty());
    assert_eq!(began, [bytes(|command| down(command, 1, 30, 40))]);
}

#[test]
fn a_restart_of_a_busy_contact_ends_and_begins_it_in_two_commands() {
    let mut screen = Screen::new();
    screen.position(0, 10, 20);
    screen.table.touch(0);
    screen.finish();

    screen.position(0, 50, 60);
    screen.table.restart(0);
    let restarted = screen.finish();

    assert_eq!(
        restarted,
        [
            bytes(|command| {
                command.up(0).expect("room");
            }),
            bytes(|command| down(command, 0, 50, 60)),
        ]
    );
}

#[test]
fn a_lift_ends_a_busy_touch_and_changes_nothing_for_a_free_contact() {
    let mut screen = Screen::new();
    screen.position(0, 10, 20);
    screen.table.touch(0);
    screen.finish();

    screen.table.lift(0);
    screen.table.lift(1);
    let lifted = screen.finish();
    screen.table.lift(0);
    let again = screen.finish();

    assert_eq!(
        lifted,
        [bytes(|command| {
            command.up(0).expect("room");
        })]
    );
    assert!(again.is_empty());
}

#[test]
fn a_cancel_cancels_busy_touches_and_drops_the_frame() {
    let mut screen = Screen::new();
    screen.position(0, 10, 20);
    screen.table.touch(0);
    screen.finish();

    screen.position(1, 30, 40);
    screen.table.touch(1);
    let cancelled = screen.cancel();
    let after = screen.finish();
    screen.table.touch(0);
    let began = screen.finish();

    assert_eq!(
        cancelled,
        [bytes(|command| {
            command.cancel(0).expect("room");
        })]
    );
    assert!(after.is_empty());
    assert_eq!(began, [bytes(|command| down(command, 0, 10, 20))]);
}

#[test]
fn contacts_and_axes_outside_the_table_are_ignored() {
    let mut screen = Screen::new();

    screen.table.axis(0, 2, 5);
    screen.table.axis(CONTACTS, X, 5);
    screen.table.touch(CONTACTS);
    let ignored = screen.finish();

    assert!(ignored.is_empty());
}
