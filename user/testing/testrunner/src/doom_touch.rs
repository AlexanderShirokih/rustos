//! Контроллер Doom на синтетических кадрах: независимые пальцы, источники и поворот.

use alloc::vec::Vec;

use display_abi::{Area, DisplayDimension};
use doomgeneric::{Button, Control, InputState, KeyEvent, Layout, Mode, Point};
use input_abi::{
    AxisDecl, AxisList, Binding, Command, DeviceDescriptor, DeviceLayout, DeviceNumber, KeyList,
    Record, RecordBuf, Tracker, Usage,
};

fn layout(width: u32, height: u32) -> Layout {
    Layout::new(DisplayDimension { width, height }).expect("screen fits")
}

fn device() -> DeviceLayout {
    let mut keys = KeyList::new();
    keys.push(Usage::new(7, 0xe0)).expect("Ctrl");
    keys.push(Usage::new(7, 0xe4)).expect("right Ctrl");
    let mut axes = AxisList::new();
    axes.push(AxisDecl::absolute(Usage::X, 0, 65535))
        .expect("X");
    axes.push(AxisDecl::absolute(Usage::Y, 0, 65535))
        .expect("Y");
    DeviceDescriptor::new(
        "touch and keyboard",
        Binding::Screen,
        &keys,
        &AxisList::new(),
        3,
        &axes,
    )
    .expect("descriptor")
    .layout()
    .expect("layout")
}

fn button(layout: Layout, control: Control) -> Button {
    layout
        .buttons(Mode::Game, false)
        .into_iter()
        .find(|b| b.control == control)
        .expect("button")
}

fn center(button: Button) -> Point {
    Point {
        x: button.area.x + button.area.width / 2,
        y: button.area.y + button.area.height / 2,
    }
}

fn physical(layout: Layout, point: Point) -> Point {
    let area = layout.physical_area(Area {
        x: point.x,
        y: point.y,
        width: 1,
        height: 1,
    });
    Point {
        x: area.x,
        y: area.y,
    }
}

struct Input {
    state: InputState,
    layout: Layout,
    tracker: Tracker,
    number: DeviceNumber,
    events: Vec<KeyEvent>,
}

impl Input {
    fn new(layout: Layout) -> Self {
        let number = DeviceNumber::new(1).expect("number");
        let mut state = InputState::new(layout);
        state.feed(Record::device_added(number), |_| Some(device()), |_| {});
        Self {
            state,
            layout,
            number,
            tracker: Tracker::new(number, device()),
            events: Vec::new(),
        }
    }

    fn send(&mut self, command: &Command) {
        let mut records = RecordBuf::new();
        self.tracker
            .apply(command.as_bytes(), None, 0, &mut records)
            .expect("valid command");
        for record in records.records() {
            self.record(record);
        }
    }

    fn record(&mut self, record: Record) {
        self.state
            .feed(record, |_| Some(device()), |event| self.events.push(event));
    }

    fn axes(&self, command: &mut Command, slot: u16, point: Point) {
        let point = physical(self.layout, point);
        let panel = self.layout.panel();
        let axis = |value, length| {
            (i64::from(value) * 65535 + i64::from(length) - 2) / i64::from(length - 1)
        };
        command
            .contact_axis(slot, 0, axis(point.x, panel.width))
            .expect("X");
        command
            .contact_axis(slot, 1, axis(point.y, panel.height))
            .expect("Y");
    }

    fn down(&mut self, slot: u16, point: Point) {
        let mut command = Command::new();
        command.down(slot).expect("down");
        self.axes(&mut command, slot, point);
        self.send(&command);
    }

    fn move_to(&mut self, slot: u16, point: Point) {
        let mut command = Command::new();
        self.axes(&mut command, slot, point);
        self.send(&command);
    }

    fn up(&mut self, slot: u16, cancel: bool) {
        let mut command = Command::new();
        if cancel {
            command.cancel(slot).expect("cancel");
        } else {
            command.up(slot).expect("up");
        }
        self.send(&command);
    }

    fn expect(&mut self, pairs: &[(u8, bool)]) {
        assert_eq!(
            self.events,
            pairs
                .iter()
                .map(|&(key, pressed)| KeyEvent { key, pressed })
                .collect::<Vec<_>>()
        );
        self.events.clear();
    }
}

#[test_harness::test]
fn doom_touch_moves_fires_and_turns_with_two_fingers() {
    let layout = layout(1080, 2340);
    let mut input = Input::new(layout);
    let mut movement = center(button(layout, Control::Move));
    movement.y -= 100;
    input.down(0, movement);
    input.expect(&[(0xad, true)]);
    let mut fire = center(button(layout, Control::Fire));
    input.down(1, fire);
    input.expect(&[(0xa3, true)]);
    fire.x += 100;
    input.move_to(1, fire);
    input.expect(&[]);
    assert!(input.state.take_turn() > 0);
    input.up(1, false);
    input.expect(&[(0xa3, false)]);
    input.up(0, false);
    input.expect(&[(0xad, false)]);
}

#[test_harness::test]
fn doom_touch_captures_the_pad_and_preserves_an_unchanged_axis() {
    let layout = layout(1280, 800);
    let mut input = Input::new(layout);
    let point = center(button(layout, Control::Turn));
    input.down(0, point);
    input.expect(&[]);
    input.move_to(
        0,
        Point {
            x: point.x + 5,
            ..point
        },
    );
    input.expect(&[]);
    let use_button = center(button(layout, Control::Key(0xa2)));
    input.move_to(
        0,
        Point {
            x: use_button.x + 100,
            y: use_button.y,
        },
    );
    input.expect(&[]);
    assert!(input.state.take_turn() > 0);
    let mut command = Command::new();
    let raw = i64::from(point.x) * 65535 / 1279;
    command.contact_axis(0, 0, raw).expect("only X changes");
    input.send(&command);
    assert!(input.state.take_turn() < 0);
    input.up(0, true);
    input.expect(&[]);
    input.down(0, use_button);
    input.expect(&[(0xa2, true)]);
    input.up(0, true);
    input.expect(&[(0xa2, false)]);
}

#[test_harness::test]
fn doom_touch_combines_keyboard_aliases_contacts_and_devices() {
    let layout = layout(1280, 800);
    let mut input = Input::new(layout);
    let mut command = Command::new();
    command.press(0).expect("Ctrl");
    command.press(1).expect("right Ctrl");
    input.send(&command);
    input.expect(&[(0xa3, true)]);
    input.down(0, center(button(layout, Control::Fire)));
    input.expect(&[]);
    let mut command = Command::new();
    command.release(0).expect("release Ctrl");
    input.send(&command);
    input.expect(&[]);
    input.up(0, true);
    input.expect(&[]);
    let other = DeviceNumber::new(2).expect("second device");
    input.record(Record::device_added(other));
    input.record(Record::key(Some(other), 0, true));
    input.record(Record::frame(Some(other), 0));
    input.expect(&[]);
    input.record(Record::device_removed(input.number));
    input.expect(&[]);
    input.record(Record::device_removed(other));
    input.expect(&[(0xa3, false)]);
}

#[test_harness::test]
fn doom_touch_resync_releases_and_snapshot_restarts_the_gesture() {
    let layout = layout(1280, 800);
    let mut input = Input::new(layout);
    let mut fire = center(button(layout, Control::Fire));
    input.down(0, fire);
    fire.x -= 80;
    input.move_to(0, fire);
    input.expect(&[(0xa3, true)]);
    assert!(input.state.take_turn() < 0);
    input.record(Record::resync());
    input.expect(&[(0xa3, false)]);
    input.record(Record::device_added(input.number));
    let mut snapshot = RecordBuf::new();
    input.tracker.snapshot(&mut snapshot);
    for record in snapshot.records() {
        input.record(record);
    }
    input.expect(&[(0xa3, true)]);
    input.state.release_all(|event| input.events.push(event));
    input.expect(&[(0xa3, false)]);
}

#[test_harness::test]
fn doom_touch_button_centers_and_rectangles_match_both_orientations() {
    for layout in [layout(1080, 2340), layout(1280, 800)] {
        for button in layout.buttons(Mode::Game, false) {
            let point = center(button);
            assert_eq!(layout.logical(physical(layout, point)), point);
            let physical = layout.physical_area(button.area);
            layout
                .panel()
                .check_area(physical)
                .expect("button inside panel");
            let mut input = Input::new(layout);
            input.down(0, point);
            if let Control::Key(key) = button.control {
                input.expect(&[(key, true)]);
            }
        }
    }
}

#[test_harness::test]
fn doom_touch_menu_cancels_old_fingers_without_accepting() {
    let layout = layout(1080, 2340);
    let mut input = Input::new(layout);
    let fire = center(button(layout, Control::Fire));
    input.down(0, fire);
    input.expect(&[(0xa3, true)]);
    input.state.set_mode(Mode::Menu, |e| input.events.push(e));
    input.expect(&[(0xa3, false)]);
    input.move_to(0, fire);
    input.expect(&[]);
    input.up(0, false);
    input.down(0, fire);
    input.expect(&[(13, true)]);
    input.state.set_mode(Mode::Game, |e| input.events.push(e));
    input.expect(&[(13, false)]);
    input.up(0, false);
    let mut command = Command::new();
    command.press(0).expect("Ctrl");
    input.send(&command);
    input.expect(&[(0xa3, true)]);
    input.state.set_mode(Mode::Menu, |e| input.events.push(e));
    input.expect(&[]);
    command = Command::new();
    command.release(0).expect("Ctrl released");
    input.send(&command);
    input.expect(&[(0xa3, false)]);
}

#[test_harness::test]
fn doom_touch_menu_uses_dominant_axis_and_repeats_navigation() {
    let layout = layout(1080, 2340);
    let mut input = Input::new(layout);
    input.state.set_mode(Mode::Menu, |e| input.events.push(e));
    let middle = center(button(layout, Control::Move));
    input.down(
        0,
        Point {
            x: middle.x + 80,
            y: middle.y + 10,
        },
    );
    input.expect(&[(0xae, true)]);
    input.move_to(
        0,
        Point {
            x: middle.x + 80,
            y: middle.y + 100,
        },
    );
    input.expect(&[(0xae, false), (0xaf, true)]);
    input.state.tick(0, |e| input.events.push(e));
    input.state.tick(349, |e| input.events.push(e));
    input.expect(&[]);
    input.state.tick(350, |e| input.events.push(e));
    input.expect(&[(0xaf, true)]);
    input.state.tick(470, |e| input.events.push(e));
    input.expect(&[(0xaf, true)]);
    input.move_to(0, middle);
    input.expect(&[(0xaf, false)]);
    let back = layout
        .buttons(Mode::Menu, false)
        .into_iter()
        .find(|b| b.label == "BACK")
        .expect("back");
    input.down(1, center(back));
    input.expect(&[(0x7f, true)]);
}

#[test_harness::test]
fn doom_touch_confirm_and_text_entry_use_engine_keys() {
    let layout = layout(1080, 2340);
    let mut input = Input::new(layout);
    input
        .state
        .set_mode(Mode::Confirm, |e| input.events.push(e));
    for (label, key) in [("YES", b'y'), ("NO", b'n')] {
        let action = layout
            .buttons(Mode::Confirm, false)
            .into_iter()
            .find(|b| b.label == label)
            .expect("answer");
        input.down(0, center(action));
        input.expect(&[(key, true)]);
        input.up(0, false);
        input.expect(&[(key, false)]);
    }
    input.state.set_mode(Mode::Text, |e| input.events.push(e));
    let back = layout
        .buttons(Mode::Text, false)
        .into_iter()
        .find(|b| b.label == "BACK")
        .expect("cancel text");
    input.down(0, center(back));
    input.expect(&[(27, true)]);
}

#[test_harness::test]
fn doom_touch_weapon_panel_and_round_hit_regions() {
    let tiny = layout(3, 4);
    for button in tiny.buttons(Mode::Game, true) {
        tiny.panel()
            .check_area(tiny.physical_area(button.area))
            .expect("clipped on the smallest panel");
    }

    let layout = layout(1080, 2340);
    let mut input = Input::new(layout);
    let selector = button(layout, Control::Weapons);
    input.down(0, center(selector));
    assert!(input.state.controls().weapons);
    input.up(0, false);
    let weapon = layout
        .buttons(Mode::Game, true)
        .into_iter()
        .find(|b| b.control == Control::Key(b'3'))
        .expect("weapon");
    input.down(0, center(weapon));
    input.expect(&[(b'3', true)]);
    assert!(!input.state.controls().weapons);
    let left = button(layout, Control::Move);
    let right = button(layout, Control::Fire);
    assert_eq!(center(left).y, center(right).y);
    assert_eq!(center(left).x + center(right).x, 2340);
    assert!(!left.contains(Point {
        x: left.area.x,
        y: left.area.y
    }));
    input.up(0, false);
    input.down(
        0,
        Point {
            x: left.area.x,
            y: left.area.y,
        },
    );
    input.expect(&[(b'3', false)]);
}

#[test_harness::test]
fn doom_touch_back_closes_the_main_menu_without_a_second_menu_button() {
    let layout = layout(1080, 2340);
    let mut input = Input::new(layout);
    for mode in [Mode::Menu, Mode::MainMenu, Mode::Confirm, Mode::Text] {
        assert!(
            layout
                .buttons(mode, false)
                .iter()
                .all(|b| b.label != "MENU")
        );
    }
    input
        .state
        .set_mode(Mode::MainMenu, |e| input.events.push(e));
    let back = layout
        .buttons(Mode::MainMenu, false)
        .into_iter()
        .find(|b| b.label == "BACK")
        .expect("main menu back");
    input.down(0, center(back));
    input.expect(&[(27, true)]);
    input.up(0, false);
    input.expect(&[(27, false)]);
}

#[test_harness::test]
fn doom_touch_controls_follow_devices_with_screen_contacts() {
    let pointer = |binding, contacts| {
        let mut axes = AxisList::new();
        axes.push(AxisDecl::absolute(Usage::X, 0, 65535))
            .expect("X");
        axes.push(AxisDecl::absolute(Usage::Y, 0, 65535))
            .expect("Y");
        let (device_axes, contact_axes) = if contacts == 0 {
            (axes, AxisList::new())
        } else {
            (AxisList::new(), axes)
        };
        DeviceDescriptor::new(
            "pointer",
            binding,
            &KeyList::new(),
            &device_axes,
            contacts,
            &contact_axes,
        )
        .expect("descriptor")
        .layout()
        .expect("layout")
    };
    let mut state = InputState::new(layout(1280, 800));
    assert!(!state.controls().touch);
    let tablet = DeviceNumber::new(1).expect("tablet");
    state.feed(
        Record::device_added(tablet),
        |_| Some(pointer(Binding::Screen, 0)),
        |_| {},
    );
    assert!(!state.controls().touch);
    let pad = DeviceNumber::new(2).expect("pad");
    state.feed(
        Record::device_added(pad),
        |_| Some(pointer(Binding::None, 2)),
        |_| {},
    );
    assert!(!state.controls().touch);
    let touch = DeviceNumber::new(3).expect("touch");
    state.feed(
        Record::device_added(touch),
        |_| Some(pointer(Binding::Screen, 2)),
        |_| {},
    );
    assert!(state.controls().touch);
    state.feed(Record::device_removed(touch), |_| None, |_| {});
    assert!(!state.controls().touch);
}
