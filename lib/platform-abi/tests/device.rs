#![allow(clippy::expect_used, clippy::unwrap_in_result)]

use ipc::{
    IntoWireValue, WireValue,
    wire::{FieldCursor, IpcError, MessageBuf},
};
use platform_abi::{DEVICE_ID_MAX, DeviceId};

const FIELD_ID: u8 = 1;

#[test]
fn a_device_id_is_an_absolute_path_of_names_within_the_limit() {
    let longest = format!("/{}", "n".repeat(DEVICE_ID_MAX - 1));
    let past = format!("/{}", "n".repeat(DEVICE_ID_MAX));
    let valid = ["/pl031@9010000", "/soc/serial@1000", longest.as_str()];
    let invalid = [
        "",
        "/",
        "soc",
        "/soc/",
        "//soc",
        "/soc//serial",
        past.as_str(),
    ];

    let accepted = valid.map(|path| DeviceId::new(path).map(|id| id.as_str().to_owned()));
    let refused = invalid.map(|path| DeviceId::new(path).is_none());

    assert_eq!(accepted, valid.map(|path| Some(path.to_owned())));
    assert_eq!(refused, [true; 7]);
}

fn read_field(written: impl IntoWireValue<()>) -> Result<String, IpcError> {
    let mut frame = MessageBuf::<()>::new();
    written.write_into(&mut frame, FIELD_ID).expect("fits");
    frame.finish().expect("fits");
    let mut cursor = FieldCursor::new(frame.as_bytes());
    let field = cursor.next_field().expect("valid").expect("present");
    DeviceId::from_field(field.data, &mut ()).map(|id| id.as_str().to_owned())
}

#[test]
fn a_device_id_field_reads_back_its_path_and_refuses_a_path_outside_the_rule() {
    let id = DeviceId::new("/soc/serial@1000").expect("valid path");

    assert_eq!(read_field(id), Ok("/soc/serial@1000".to_owned()));
    assert_eq!(read_field("soc/serial"), Err(IpcError::InvalidValue));
    assert_eq!(read_field("/soc//serial"), Err(IpcError::InvalidValue));
}
