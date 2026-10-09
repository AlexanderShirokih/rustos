#![allow(clippy::expect_used)]

use input_abi::{
    AXES_MAX, AxisDecl, AxisList, Binding, CONTACTS_MAX, DEVICE_NAME_MAX, DescriptorError,
    DeviceDescriptor, DeviceLayout, KEYS_MAX, KeyList, Usage,
};
use ipc::{
    IntoWireValue, WireValue,
    wire::{FieldCursor, MessageBuf},
};

const FIELD_ID: u8 = 1;

struct Spec<'a> {
    binding: Binding,
    keys: &'a [Usage],
    device_axes: &'a [AxisDecl],
    max_contacts: u16,
    contact_axes: &'a [AxisDecl],
}

const NONE: Spec<'static> = Spec {
    binding: Binding::None,
    keys: &[],
    device_axes: &[],
    max_contacts: 0,
    contact_axes: &[],
};

fn keys(usages: &[Usage]) -> KeyList {
    let mut list = KeyList::new();
    for &usage in usages {
        list.push(usage).expect("room");
    }
    list
}

fn keyboard(count: usize) -> Vec<Usage> {
    (0..count)
        .map(|id| Usage::new(0x07, u16::try_from(id).expect("small")))
        .collect()
}

fn axes(decls: &[AxisDecl]) -> AxisList {
    let mut list = AxisList::new();
    for &decl in decls {
        list.push(decl).expect("room");
    }
    list
}

fn layout(spec: &Spec<'_>) -> Result<DeviceLayout, DescriptorError> {
    let keys = keys(spec.keys);
    let device_axes = axes(spec.device_axes);
    let contact_axes = axes(spec.contact_axes);
    DeviceDescriptor::new(
        "device",
        spec.binding,
        &keys,
        &device_axes,
        spec.max_contacts,
        &contact_axes,
    )
    .expect("short name")
    .layout()
}

fn refusal(spec: &Spec<'_>) -> DescriptorError {
    layout(spec).expect_err("refused")
}

fn raw(keys: &[u8], device_axes: &[u8]) -> Result<DeviceLayout, DescriptorError> {
    DeviceDescriptor {
        name: "device",
        binding: Binding::None,
        keys,
        device_axes,
        max_contacts: 0,
        contact_axes: &[],
    }
    .layout()
}

fn position(min: i32, max: i32) -> [AxisDecl; 2] {
    [
        AxisDecl::absolute(Usage::X, min, max),
        AxisDecl::absolute(Usage::Y, min, max),
    ]
}

#[test]
fn a_mouse_is_accepted() {
    let mouse = Spec {
        keys: &[Usage::PRIMARY, Usage::SECONDARY],
        device_axes: &[AxisDecl::relative(Usage::X), AxisDecl::relative(Usage::Y)],
        ..NONE
    };

    assert!(layout(&mouse).is_ok());
}

#[test]
fn a_ten_finger_touchscreen_snapshot_takes_31_records() {
    let touchscreen = Spec {
        binding: Binding::Screen,
        max_contacts: 10,
        contact_axes: &position(0, 1079),
        ..NONE
    };

    let layout = layout(&touchscreen).expect("accepted");

    assert_eq!(layout.snapshot_records_max(), 31);
}

#[test]
fn a_touchpad_is_accepted() {
    let touchpad = Spec {
        keys: &[Usage::PRIMARY],
        max_contacts: 5,
        contact_axes: &position(0, 4095),
        ..NONE
    };

    assert!(layout(&touchpad).is_ok());
}

#[test]
fn a_120_key_keyboard_snapshot_takes_121_records() {
    let usages = keyboard(120);
    let keyboard = Spec {
        keys: &usages,
        ..NONE
    };

    let layout = layout(&keyboard).expect("accepted");

    assert_eq!(layout.snapshot_records_max(), 121);
}

#[test]
fn a_snapshot_past_the_message_record_limit_is_refused() {
    let usages = keyboard(120);
    let device_axes: Vec<AxisDecl> = (10..18)
        .map(|id| AxisDecl::absolute(Usage::new(0x01, id), 0, 1))
        .collect();
    let contact_axes: Vec<AxisDecl> = (0..8)
        .map(|id| AxisDecl::absolute(Usage::new(0x01, id), 0, 1))
        .collect();
    let crowded = Spec {
        keys: &usages,
        device_axes: &device_axes,
        max_contacts: CONTACTS_MAX,
        contact_axes: &contact_axes,
        ..NONE
    };

    assert_eq!(refusal(&crowded), DescriptorError::TooLarge);
}

#[test]
fn a_relative_contact_axis_is_refused() {
    let spec = Spec {
        max_contacts: 1,
        contact_axes: &[AxisDecl::relative(Usage::X)],
        ..NONE
    };

    assert_eq!(refusal(&spec), DescriptorError::Malformed);
}

#[test]
fn contacts_and_contact_axes_come_together() {
    let bare_contacts = Spec {
        max_contacts: 2,
        ..NONE
    };
    let bare_axes = Spec {
        contact_axes: &position(0, 10),
        ..NONE
    };

    assert_eq!(refusal(&bare_contacts), DescriptorError::Inconsistent);
    assert_eq!(refusal(&bare_axes), DescriptorError::Inconsistent);
}

#[test]
fn contacts_past_the_model_limit_are_refused() {
    let spec = Spec {
        max_contacts: CONTACTS_MAX + 1,
        contact_axes: &position(0, 10),
        ..NONE
    };

    assert_eq!(refusal(&spec), DescriptorError::TooLarge);
}

#[test]
fn axes_past_the_model_limit_are_refused() {
    let mut list = axes(
        &(0..AXES_MAX)
            .map(|id| u16::try_from(id).expect("small"))
            .map(|id| AxisDecl::relative(Usage::new(0x01, id)))
            .collect::<Vec<_>>(),
    );

    assert_eq!(
        list.push(AxisDecl::relative(Usage::X)),
        Err(DescriptorError::TooLarge)
    );
}

#[test]
fn keys_past_the_model_limit_are_refused() {
    let mut list = keys(&keyboard(KEYS_MAX));
    let mut crowded = list.as_bytes().to_vec();
    crowded.extend_from_slice(&[0; 4]);

    assert_eq!(list.push(Usage::POWER), Err(DescriptorError::TooLarge));
    assert_eq!(raw(&crowded, &[]).err(), Some(DescriptorError::TooLarge));
}

#[test]
fn a_name_past_the_limit_is_refused() {
    let name = "n".repeat(DEVICE_NAME_MAX + 1);
    let empty = AxisList::new();
    let descriptor = DeviceDescriptor {
        name: &name,
        binding: Binding::None,
        keys: &[],
        device_axes: &[],
        max_contacts: 0,
        contact_axes: &[],
    };

    assert!(
        DeviceDescriptor::new(&name, Binding::None, &KeyList::new(), &empty, 0, &empty).is_none()
    );
    assert_eq!(descriptor.layout().err(), Some(DescriptorError::TooLarge));
}

#[test]
fn a_screen_binding_without_absolute_position_is_refused() {
    let relative = Spec {
        binding: Binding::Screen,
        device_axes: &[AxisDecl::relative(Usage::X), AxisDecl::relative(Usage::Y)],
        ..NONE
    };
    let split = Spec {
        binding: Binding::Screen,
        device_axes: &[AxisDecl::absolute(Usage::X, 0, 10)],
        max_contacts: 1,
        contact_axes: &[AxisDecl::absolute(Usage::Y, 0, 10)],
        ..NONE
    };

    assert_eq!(refusal(&relative), DescriptorError::Inconsistent);
    assert_eq!(refusal(&split), DescriptorError::Inconsistent);
}

#[test]
fn absolute_position_on_both_device_and_contact_is_refused() {
    let spec = Spec {
        device_axes: &[AxisDecl::absolute(Usage::X, 0, 10)],
        max_contacts: 1,
        contact_axes: &position(0, 10),
        ..NONE
    };

    assert_eq!(refusal(&spec), DescriptorError::Inconsistent);
}

#[test]
fn a_repeated_axis_usage_is_refused() {
    let spec = Spec {
        device_axes: &[AxisDecl::relative(Usage::X), AxisDecl::relative(Usage::X)],
        ..NONE
    };

    assert_eq!(refusal(&spec), DescriptorError::Malformed);
}

#[test]
fn a_repeated_key_usage_is_refused() {
    let spec = Spec {
        keys: &[Usage::POWER, Usage::ENTER, Usage::POWER],
        ..NONE
    };

    assert_eq!(refusal(&spec), DescriptorError::Malformed);
}

#[test]
fn a_key_on_a_vendor_page_is_accepted() {
    let spec = Spec {
        keys: &[Usage::new(0xFF00, 0x0001)],
        ..NONE
    };

    let layout = layout(&spec).expect("accepted");

    assert_eq!(layout.key(0), Some(Usage::new(0xFF00, 0x0001)));
}

#[test]
fn an_inverted_absolute_range_is_refused() {
    let spec = Spec {
        device_axes: &[AxisDecl::absolute(Usage::X, 5, 4)],
        ..NONE
    };

    assert_eq!(refusal(&spec), DescriptorError::Malformed);
}

#[test]
fn a_relative_axis_with_a_range_is_refused() {
    let mut bytes = axes(&[AxisDecl::relative(Usage::X)]).as_bytes().to_vec();
    bytes[8..12].copy_from_slice(&1i32.to_le_bytes());

    assert_eq!(raw(&[], &bytes).err(), Some(DescriptorError::Malformed));
}

#[test]
fn lists_that_do_not_split_into_entries_are_refused() {
    let mut unknown_mode = axes(&[AxisDecl::relative(Usage::X)]).as_bytes().to_vec();
    unknown_mode[4] = 9;

    assert_eq!(raw(&[], &[0; 17]).err(), Some(DescriptorError::Malformed));
    assert_eq!(
        raw(&[], &unknown_mode).err(),
        Some(DescriptorError::Malformed)
    );
    assert_eq!(raw(&[0; 3], &[]).err(), Some(DescriptorError::Malformed));
}

#[test]
fn a_descriptor_round_trips_through_an_ipc_field() {
    let keys = keys(&[Usage::POWER, Usage::VOLUME_UP]);
    let device_axes = axes(&[AxisDecl::relative(Usage::new(0x01, 0x38))]);
    let contact_axes = axes(&position(-10, 2000));
    let descriptor = DeviceDescriptor::new(
        "touch",
        Binding::Screen,
        &keys,
        &device_axes,
        3,
        &contact_axes,
    )
    .expect("short name");

    let mut frame = MessageBuf::<()>::new();
    descriptor.write_into(&mut frame, FIELD_ID).expect("fits");
    frame.finish().expect("fits");
    let mut cursor = FieldCursor::new(frame.as_bytes());
    let field = cursor.next_field().expect("valid").expect("present");
    let read = DeviceDescriptor::from_field(field.data, &mut ()).expect("decoded");

    assert_eq!(read, descriptor);
    assert!(read.layout().is_ok());
}
