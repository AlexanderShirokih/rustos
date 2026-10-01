#![no_main]
#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::missing_asserts_for_indexing,
    clippy::cast_possible_truncation,
    clippy::unwrap_in_result,
    clippy::panic,
    clippy::std_instead_of_core
)]

use ipc::{
    WireValue,
    wire::{FieldCursor, FrameParts, Header, value},
};
use libfuzzer_sys::fuzz_target;

const FIELD_LIMIT: usize = 256;

ipc::wire_enum! {
    enum Wide {
        First = 1,
        Last = 65535,
    }
}

ipc::wire_enum! {
    enum Narrow: u8 {
        First = 1,
        Last = 255,
    }
}

fuzz_target!(|data: &[u8]| {
    let Ok(FrameParts { body, .. }) = Header::split(data) else {
        return;
    };

    let mut cursor = FieldCursor::new(body);
    for _ in 0..FIELD_LIMIT {
        let Ok(Some(field)) = cursor.next_field() else {
            break;
        };
        let _ = (
            value::decode_u8(field.data),
            value::decode_u16(field.data),
            value::decode_u32(field.data),
            value::decode_u64(field.data),
            value::decode_i8(field.data),
            value::decode_i16(field.data),
            value::decode_i32(field.data),
            value::decode_i64(field.data),
            value::decode_bool(field.data),
            Wide::from_field(field.data, &mut ()),
            Narrow::from_field(field.data, &mut ()),
            value::decode_str(field.data),
            value::decode_port_index(field.data),
        );
    }
});
