//! Драйвер `driver-virtio-input`: дескриптор и команды на синтетической конфигурации и
//! устройства ввода QEMU `virt` за шиной `virtio-mmio`.

use alloc::vec::Vec;
use core::convert::Infallible;

use component_abi::RequirementDeclaration;
use driver_virtio_input::{Config, Device};
use input_abi::{AxisDecl, Binding, DeviceLayout, KeyList, Kind, Usage, input};
use ipc::{CallError, Protocol};
use runtime::Timeout;
use virtio::{
    DeviceType,
    input::{AbsInfo, Event, SECTION_BYTES, kind, select, sync},
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

const INPUT: &str = "input";
const DRIVER: &str = "driver-virtio-input";
const QEMU_INPUT_DEVICES: usize = 3;
const DEVICE_WAIT: Timeout = Timeout::from_secs(15);
const NO_DEVICE: CallError<VirtioMmioError> = CallError::Remote(VirtioMmioError::NoDevice);

struct Synthetic {
    name: &'static [u8],
    keys: &'static [u16],
    relative: &'static [u16],
    absolute: &'static [u16],
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
        let range = AbsInfo {
            min: 0,
            max: TABLET_MAX,
        };
        Ok(self.absolute.contains(&u16::from(axis)).then_some(range))
    }
}

fn device(name: &'static [u8], keys: &'static [u16], absolute: &'static [u16]) -> Device {
    let config = &mut Synthetic {
        name,
        keys,
        relative: &[],
        absolute,
    };
    Device::describe(config).unwrap_or_else(|never| match never {})
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
        layouts.extend(added.filter_map(|record| consumer.describe(record.device).ok()));
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

    test_harness::check_eq!(layouts.len(), QEMU_INPUT_DEVICES);
    test_harness::check!(tablet);
    test_harness::check!(mouse);
    test_harness::check!(keyboard);
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
