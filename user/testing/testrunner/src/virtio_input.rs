//! Драйвер `driver-virtio-input`: дескриптор и команды на синтетической конфигурации и
//! устройства ввода QEMU `virt` за шиной `virtio-mmio`.

use alloc::vec::Vec;
use core::convert::Infallible;

use component_abi::RequirementDeclaration;
use driver_virtio_input::{Config, Device};
use input_abi::{AxisDecl, Binding, CONTACTS_MAX, DeviceLayout, KeyList, Kind, Usage, input};
use ipc::{CallError, Protocol};
use runtime::Timeout;
use virtio::{
    DeviceType,
    input::{AbsInfo, Event, SECTION_BYTES, abs, kind, select, sync},
};
use virtio_driver::open_slot;
use virtio_mmio_abi::VirtioMmioError;

use crate::{
    composition::{Import, pause},
    input_hub::{command, consumer},
    rootkeeper_system::{
        FIRST_VIRTIO, POLL_ATTEMPTS, POLL_INTERVAL, finish, platform_describes, with_drivers,
    },
    virtio_mmio::{BUS_IMPORT, bus_session},
};

const KEY_A: u16 = 30;
const KEY_B: u16 = 48;
const KEY_C: u16 = 46;
const KEY_UNTRANSLATED: u16 = 240;
const BTN_LEFT: u16 = 0x110;
const AXIS_X: u16 = 0;
const AXIS_Y: u16 = 1;

const TABLET_MAX: i32 = 0x7fff;
const QEMU_TOUCH_CONTACTS: u16 = 11;
const TOUCH_SLOTS: i32 = 2;
const END_TOUCH: i32 = -1;

const INPUT: &str = "input";
const DRIVER: &str = "driver-virtio-input";
const QEMU_INPUT_DEVICES: usize = 4;
const DEVICE_WAIT: Timeout = Timeout::from_secs(15);
const NO_DEVICE: CallError<VirtioMmioError> = CallError::Remote(VirtioMmioError::NoDevice);

struct Synthetic {
    name: &'static [u8],
    keys: &'static [u16],
    relative: &'static [u16],
    absolute: &'static [u16],
    slots: i32,
}

impl Config for Synthetic {
    type Error = Infallible;

    fn section(
        &mut self,
        select: u8,
        subsel: u8,
        destination: &mut [u8; SECTION_BYTES],
    ) -> Result<usize, Infallible> {
        destination.fill(0);
        let codes = match (select, u16::from(subsel)) {
            (select::ID_NAME, _) => {
                destination[..self.name.len()].copy_from_slice(self.name);
                return Ok(self.name.len());
            }
            (select::EV_BITS, kind::KEY) => self.keys,
            (select::EV_BITS, kind::REL) => self.relative,
            (select::EV_BITS, kind::ABS) => self.absolute,
            _ => &[],
        };
        for code in codes {
            destination[usize::from(code / 8)] |= 1 << (code % 8);
        }
        let length = codes.iter().max().map_or(0, |code| code / 8 + 1);
        Ok(usize::from(length))
    }

    fn abs_info(&mut self, axis: u8) -> Result<Option<AbsInfo>, Infallible> {
        let max = if u16::from(axis) == abs::MT_SLOT {
            self.slots - 1
        } else {
            TABLET_MAX
        };
        let range = AbsInfo { min: 0, max };
        Ok(self.absolute.contains(&u16::from(axis)).then_some(range))
    }
}

fn device(name: &'static [u8], keys: &'static [u16], absolute: &'static [u16]) -> Device {
    let config = &mut Synthetic {
        name,
        keys,
        relative: &[],
        absolute,
        slots: 0,
    };
    Device::describe(config).unwrap_or_else(|never| match never {})
}

fn touchscreen(slots: i32) -> Device {
    let config = &mut Synthetic {
        name: b"touchscreen",
        keys: &[],
        relative: &[],
        absolute: &[
            AXIS_X,
            AXIS_Y,
            abs::MT_SLOT,
            abs::MT_POSITION_X,
            abs::MT_POSITION_Y,
            abs::MT_TRACKING_ID,
        ],
        slots,
    };
    Device::describe(config).unwrap_or_else(|never| match never {})
}

fn slot(index: i32) -> Event {
    event(kind::ABS, abs::MT_SLOT, index)
}

fn tracking(id: i32) -> Event {
    event(kind::ABS, abs::MT_TRACKING_ID, id)
}

fn position(x: i32, y: i32) -> [Event; 2] {
    [
        event(kind::ABS, abs::MT_POSITION_X, x),
        event(kind::ABS, abs::MT_POSITION_Y, y),
    ]
}

fn begin(slot_index: i32, id: i32, x: i32, y: i32) -> [Event; 4] {
    let [x, y] = position(x, y);
    [slot(slot_index), tracking(id), x, y]
}

fn event(kind: u16, code: u16, value: i32) -> Event {
    Event { kind, code, value }
}

fn report() -> Event {
    event(kind::SYN, sync::REPORT, 0)
}

fn commands(device: &mut Device, events: &[Event]) -> Vec<Vec<u8>> {
    let translated = events
        .iter()
        .map(|event| device.translate(*event).expect("frame fits"));
    translated
        .flatten()
        .map(|command| command.as_bytes().to_vec())
        .collect()
}

fn frame(events: &[&[Event]]) -> Vec<Event> {
    let mut frame = events.concat();
    frame.push(report());
    frame
}

#[test_harness::test]
fn a_tablet_frame_becomes_one_command_on_a_screen_device() {
    let mut tablet = device(b"tablet\0", &[BTN_LEFT], &[AXIS_X, AXIS_Y]);

    let frame = [
        event(kind::ABS, AXIS_X, 100),
        event(kind::ABS, AXIS_Y, 200),
        event(kind::KEY, BTN_LEFT, 1),
        report(),
    ];
    let binding = tablet.descriptor().map(|descriptor| descriptor.binding);

    test_harness::check_eq!(tablet.name(), "tablet");
    test_harness::check_eq!(binding, Some(Binding::Screen));
    test_harness::check_eq!(
        commands(&mut tablet, &frame),
        [command(|c| c.axis(0, 100)?.axis(1, 200)?.press(0))
            .as_bytes()
            .to_vec()]
    );
}

#[test_harness::test]
fn a_key_repeat_or_a_release_of_a_released_key_sends_nothing() {
    let mut keyboard = device(b"keyboard", &[KEY_A], &[]);

    let events = [
        event(kind::KEY, KEY_A, 0),
        report(),
        event(kind::KEY, KEY_A, 1),
        report(),
        event(kind::KEY, KEY_A, 2),
        report(),
    ];

    test_harness::check_eq!(
        commands(&mut keyboard, &events),
        [command(|c| c.press(0)).as_bytes().to_vec()]
    );
}

#[test_harness::test]
fn dropped_events_release_the_pressed_keys() {
    let mut keyboard = device(b"keyboard", &[KEY_A, KEY_B, KEY_C], &[]);

    let events = [
        event(kind::KEY, KEY_A, 1),
        report(),
        event(kind::KEY, KEY_B, 1),
        event(kind::SYN, sync::DROPPED, 0),
        event(kind::KEY, KEY_C, 1),
        report(),
    ];

    test_harness::check_eq!(
        commands(&mut keyboard, &events),
        [
            command(|c| c.press(0)).as_bytes().to_vec(),
            command(|c| c.release(0)).as_bytes().to_vec()
        ]
    );
}

#[test_harness::test]
fn an_untranslated_code_is_not_declared_and_its_event_is_dropped() {
    let mut keyboard = device(b"keyboard", &[KEY_UNTRANSLATED, KEY_A], &[]);
    let mut keys = KeyList::new();
    keys.push(Usage::new(0x07, 0x04)).expect("key fits");

    let events = [
        event(kind::KEY, KEY_UNTRANSLATED, 1),
        event(kind::REL, AXIS_X, 1),
        report(),
    ];
    let declared = keyboard
        .descriptor()
        .map(|descriptor| descriptor.keys.to_vec());

    test_harness::check_eq!(declared, Some(keys.as_bytes().to_vec()));
    test_harness::check!(commands(&mut keyboard, &events).is_empty());
}

#[test_harness::test]
fn a_name_outside_utf8_is_replaced() {
    let device = device(&[0xff, 0xfe], &[], &[]);

    test_harness::check_eq!(device.name(), "virtio-input");
}

#[test_harness::test]
fn two_touches_of_one_frame_become_one_command_and_end_together() {
    let mut touchscreen = touchscreen(TOUCH_SLOTS);
    let layout = touchscreen
        .descriptor()
        .and_then(|descriptor| descriptor.layout().ok());
    let contact_screen = layout.as_ref().is_some_and(|layout| {
        layout.binding() == Binding::Screen
            && layout.device_axis(0).is_none()
            && layout.contact_axis(0) == Some(AxisDecl::absolute(Usage::X, 0, TABLET_MAX))
            && layout.contact_axis(1) == Some(AxisDecl::absolute(Usage::Y, 0, TABLET_MAX))
    });

    let events = [
        frame(&[&begin(0, 10, 100, 200), &begin(1, 11, 300, 400)]),
        frame(&[&[slot(0), tracking(END_TOUCH), slot(1), tracking(END_TOUCH)]]),
    ]
    .concat();

    test_harness::check!(contact_screen);
    test_harness::check_eq!(
        commands(&mut touchscreen, &events),
        [
            command(|c| {
                c.down(0)?
                    .contact_axis(0, 0, 100)?
                    .contact_axis(0, 1, 200)?
                    .down(1)?
                    .contact_axis(1, 0, 300)?
                    .contact_axis(1, 1, 400)
            })
            .as_bytes()
            .to_vec(),
            command(|c| c.up(0)?.up(1)).as_bytes().to_vec(),
        ]
    );
}

#[test_harness::test]
fn a_new_touch_on_a_busy_slot_ends_and_begins_it_in_two_commands() {
    let mut touchscreen = touchscreen(TOUCH_SLOTS);

    let [x, _] = position(150, 0);
    let events = [
        frame(&[&begin(0, 10, 100, 200)]),
        frame(&[&[tracking(12), x]]),
    ]
    .concat();

    test_harness::check_eq!(
        commands(&mut touchscreen, &events)[1..],
        [
            command(|c| c.up(0)).as_bytes().to_vec(),
            command(|c| c.down(0)?.contact_axis(0, 0, 150)?.contact_axis(0, 1, 200))
                .as_bytes()
                .to_vec(),
        ]
    );
}

#[test_harness::test]
fn dropped_events_cancel_the_touch_and_a_slot_without_a_new_touch_stays_free() {
    let mut touchscreen = touchscreen(TOUCH_SLOTS);

    let events = [
        frame(&[&begin(0, 10, 100, 200)]),
        frame(&[&[event(kind::SYN, sync::DROPPED, 0)]]),
        frame(&[&position(150, 250)]),
    ]
    .concat();

    test_harness::check_eq!(
        commands(&mut touchscreen, &events)[1..],
        [command(|c| c.cancel(0)).as_bytes().to_vec()]
    );
}

#[test_harness::test]
fn a_touch_begins_once_every_coordinate_of_its_slot_is_known() {
    let mut touchscreen = touchscreen(TOUCH_SLOTS);

    let [x, y] = position(100, 200);
    let events = [
        frame(&[&[slot(0), tracking(10)]]),
        frame(&[&[x]]),
        frame(&[&[y]]),
        frame(&[&[tracking(END_TOUCH)]]),
        frame(&[&[tracking(11)]]),
    ]
    .concat();
    let down = command(|c| c.down(0)?.contact_axis(0, 0, 100)?.contact_axis(0, 1, 200));

    test_harness::check_eq!(
        commands(&mut touchscreen, &events),
        [
            down.as_bytes().to_vec(),
            command(|c| c.up(0)).as_bytes().to_vec(),
            down.as_bytes().to_vec(),
        ]
    );
}

#[test_harness::test]
fn slots_beyond_the_model_limit_are_not_declared_and_their_events_are_dropped() {
    let beyond = i32::from(CONTACTS_MAX);
    let mut touchscreen = touchscreen(beyond + 1);
    let contacts = touchscreen
        .descriptor()
        .and_then(|descriptor| descriptor.layout().ok())
        .map(|layout| layout.max_contacts());

    let events = frame(&[&begin(beyond, 10, 100, 200), &[tracking(END_TOUCH)]]);

    test_harness::check_eq!(contacts, Some(CONTACTS_MAX));
    test_harness::check!(commands(&mut touchscreen, &events).is_empty());
}

#[test_harness::test]
fn qemu_input_devices_are_added_with_their_keys_and_axes() {
    if !platform_describes(FIRST_VIRTIO) {
        test_harness::skip!("{} is not described", FIRST_VIRTIO);
    }

    let composition = with_drivers();
    let input = Import {
        declaration: RequirementDeclaration::session(INPUT, input::Protocol::ID),
        namespace_root: None,
    };
    let importer = composition.importer(&[BUS_IMPORT, input]);
    let session = importer.session::<input::Protocol>(INPUT);
    let consumer = consumer(&session).with_wait(DEVICE_WAIT);
    let mut layouts: Vec<DeviceLayout> = Vec::new();
    while layouts.len() < QEMU_INPUT_DEVICES
        && let Ok(records) = consumer.next()
    {
        let added = records
            .records()
            .filter(|record| record.kind == Kind::DeviceAdded);
        layouts.extend(added.filter_map(|record| consumer.describe(record.device?).ok()));
    }
    let bus = bus_session(&importer);
    if layouts.is_empty() && open_slot(&bus, DeviceType::Input).err() == Some(NO_DEVICE) {
        drop((consumer, session, bus, importer));
        finish(composition);
        test_harness::skip!("no virtio-input transport");
    }

    let has_key =
        |layout: &DeviceLayout, usage| (0..).map_while(|i| layout.key(i)).any(|k| k == usage);
    let has_axis = |layout: &DeviceLayout, decl| {
        (0..)
            .map_while(|i| layout.device_axis(i))
            .any(|d| d == decl)
    };
    let tablet = layouts.iter().any(|layout| {
        has_axis(layout, AxisDecl::absolute(Usage::X, 0, TABLET_MAX))
            && has_axis(layout, AxisDecl::absolute(Usage::Y, 0, TABLET_MAX))
            && has_key(layout, Usage::PRIMARY)
    });
    let mouse = layouts.iter().any(|layout| {
        has_axis(layout, AxisDecl::relative(Usage::X))
            && has_axis(layout, AxisDecl::relative(Usage::Y))
            && has_key(layout, Usage::PRIMARY)
            && has_key(layout, Usage::SECONDARY)
    });
    let keyboard = layouts.iter().any(|layout| has_key(layout, Usage::ENTER));
    let touchscreen = layouts.iter().any(|layout| {
        layout.binding() == Binding::Screen
            && layout.max_contacts() == QEMU_TOUCH_CONTACTS
            && layout.device_axis(0).is_none()
            && layout.contact_axis(0) == Some(AxisDecl::absolute(Usage::X, 0, TABLET_MAX))
            && layout.contact_axis(1) == Some(AxisDecl::absolute(Usage::Y, 0, TABLET_MAX))
    });

    test_harness::check_eq!(layouts.len(), QEMU_INPUT_DEVICES);
    test_harness::check!(tablet);
    test_harness::check!(mouse);
    test_harness::check!(keyboard);
    test_harness::check!(touchscreen);
    // Шина должна сбросить транспорты драйвера, пока окна платформы отображены, поэтому
    // драйвер завершается до разбора композиции.
    composition.terminate(DRIVER, 0);
    let released = (0..POLL_ATTEMPTS).find_map(|_| {
        pause(POLL_INTERVAL);
        open_slot(&bus, DeviceType::Input).ok()
    });
    test_harness::check!(released.is_some());
    drop((consumer, session, released, bus, importer));
    finish(composition);
}
