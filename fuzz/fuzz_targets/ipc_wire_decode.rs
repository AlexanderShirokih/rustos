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

use ipc::wire::{FieldCursor, HEADER_SIZE, Header, value};
use libfuzzer_sys::fuzz_target;

const FIELD_LIMIT: usize = 256;

fuzz_target!(|data: &[u8]| {
    let _ = Header::decode(data);
    let Some(body) = data.get(HEADER_SIZE..) else {
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
            value::decode_discriminant(field.data),
            value::decode_str(field.data),
            value::decode_port_index(field.data),
        );
    }
});
