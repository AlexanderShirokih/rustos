//! E2E изоляции адресных пространств из EL0: у двух экземпляров одного бинаря
//! по одному и тому же VA разное содержимое.
//!
//! Слот фикстуры лежит в её `.bss`, и у обоих потомков он приходится на
//! один и тот же VA - это гарантирует сам образ.
//! Совпадение адресов проверяется явно, чтобы тест не выродился в
//! "две программы пишут по разным адресам".

use crate::{
    fixture::Fixture,
    fixture_protocol::{OP_READ_SLOT, OP_WRITE_SLOT, READ_REPLY_LEN, REPLY_OK},
};

const SENTINEL_A: u64 = 0xA1A1_A1A1_A1A1_A1A1;
const SENTINEL_B: u64 = 0xB2B2_B2B2_B2B2_B2B2;

struct Slot {
    value: u64,
    address: u64,
}

fn read_slot(fixture: &Fixture) -> Slot {
    let mut out = [0u8; READ_REPLY_LEN];
    let len = fixture.request(OP_READ_SLOT, 0, &[], &mut out);
    test_harness::check_eq!(len, READ_REPLY_LEN);
    Slot {
        value: u64::from_le_bytes(out[..8].try_into().expect("8 bytes")),
        address: u64::from_le_bytes(out[8..].try_into().expect("8 bytes")),
    }
}

#[test_harness::test]
fn two_processes_see_distinct_memory_at_same_va() {
    let a = Fixture::spawn();
    let b = Fixture::spawn();

    test_harness::check_eq!(a.request_status(OP_WRITE_SLOT, SENTINEL_A, &[]), REPLY_OK);
    test_harness::check_eq!(b.request_status(OP_WRITE_SLOT, SENTINEL_B, &[]), REPLY_OK);

    let slot_a = read_slot(&a);
    let slot_b = read_slot(&b);

    test_harness::check_eq!(slot_a.address, slot_b.address);
    test_harness::check_eq!(slot_a.value, SENTINEL_A);
    test_harness::check_eq!(slot_b.value, SENTINEL_B);
}
