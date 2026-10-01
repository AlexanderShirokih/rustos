#![allow(clippy::expect_used)]

use ipc::{
    IntoWireValue, WireValue,
    wire::{FieldCursor, IpcError, MessageBuf},
};

ipc::wire_enum! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Refusal {
        Busy = 1,
        Denied = 7,
    }
}

ipc::wire_enum! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Side: u8 {
        Left = 1,
        Right = 2,
    }
}

const FIELD_ID: u8 = 1;

fn field_of(data: &[u8]) -> MessageBuf {
    let mut buf = MessageBuf::new();
    buf.write_field(FIELD_ID, data).expect("write ok");
    buf.finish().expect("finish ok");
    buf
}

fn written(value: impl IntoWireValue<()>) -> MessageBuf {
    let mut buf = MessageBuf::new();
    value.write_into(&mut buf, FIELD_ID).expect("write ok");
    buf.finish().expect("finish ok");
    buf
}

fn read<T: for<'a> WireValue<'a, (), Owned = T>>(buf: &MessageBuf) -> Result<T, IpcError> {
    let mut cursor = FieldCursor::new(buf.as_bytes());
    let field = cursor.next_field()?.ok_or(IpcError::MissingField)?;
    T::from_field(field.data, &mut ())
}

#[test]
fn a_variant_is_written_and_read_as_its_declared_discriminant() {
    let denied = field_of(&7u16.to_le_bytes());

    assert_eq!(written(Refusal::Denied).as_bytes(), denied.as_bytes());
    assert_eq!(read(&denied), Ok(Refusal::Denied));
    assert_eq!(read(&field_of(&1u16.to_le_bytes())), Ok(Refusal::Busy));
}

#[test]
fn a_declared_u8_discriminant_takes_one_byte() {
    let right = field_of(&[2]);

    assert_eq!(written(Side::Right).as_bytes(), right.as_bytes());
    assert_eq!(read(&right), Ok(Side::Right));
    assert_eq!(u8::from(Side::Left), 1);
    assert_eq!(Side::try_from(2), Ok(Side::Right));
}

#[test]
fn a_discriminant_outside_the_declaration_is_rejected() {
    for discriminant in [0, 2, u16::MAX] {
        assert_eq!(
            read::<Refusal>(&field_of(&discriminant.to_le_bytes())),
            Err(IpcError::InvalidValue)
        );
    }
    assert_eq!(Side::try_from(3), Err(IpcError::InvalidValue));
}

#[test]
fn a_field_other_than_the_discriminant_width_is_rejected() {
    for data in [&[1][..], &[1, 0, 0]] {
        assert_eq!(
            read::<Refusal>(&field_of(data)),
            Err(IpcError::InvalidValue)
        );
    }
    assert_eq!(
        read::<Side>(&field_of(&[1, 0])),
        Err(IpcError::InvalidValue)
    );
}
