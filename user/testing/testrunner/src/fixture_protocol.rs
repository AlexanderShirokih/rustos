//! Протокол `port-fixture`: запрос-ответ поверх bootstrap-порта потомка.
//!
//! Запрос - опкод и один `u64`-аргумент. Ответ зависит от опкода. Родитель
//! всегда идёт через `call`, и у фикстуры на каждый запрос есть
//! reply-хэндл.

// Модуль подключён к обоим бинарям. Родитель кодирует запросы и читает коды
// ответов, фикстура - декодирует.
#![allow(dead_code)]

pub(crate) const OP_WRITE_SLOT: u8 = 1;
pub(crate) const OP_READ_SLOT: u8 = 2;
pub(crate) const OP_MAP_CHECK: u8 = 3;
pub(crate) const OP_EXIT: u8 = 4;
pub(crate) const OP_ALLOCATE_PAGES: u8 = 5;
pub(crate) const OP_DUPLICATE_TRANSFER_CAP: u8 = 6;
pub(crate) const OP_PROCESS_EXIT: u8 = 7;
pub(crate) const OP_HOLD_CAP: u8 = 8;
pub(crate) const OP_HOLD_SELF: u8 = 9;
pub(crate) const OP_CLOSE_CAP: u8 = 10;

pub(crate) const REQUEST_LEN: usize = 1 + 8;
pub(crate) const READ_REPLY_LEN: usize = 8 + 8;

pub(crate) const REPLY_OK: u8 = 0;
pub(crate) const REPLY_PATTERN_MISMATCH: u8 = 1;
pub(crate) const REPLY_MAP_FAILED: u8 = 2;
pub(crate) const REPLY_BUDGET_EXHAUSTED: u8 = 3;
pub(crate) const REPLY_ALLOCATE_FAILED: u8 = 4;
pub(crate) const REPLY_ACCESS_DENIED: u8 = 5;
pub(crate) const REPLY_DUPLICATE_FAILED: u8 = 6;
pub(crate) const REPLY_SPAWN_FAILED: u8 = 7;
pub(crate) const REPLY_HOLD_FAILED: u8 = 8;

pub(crate) fn encode_request(op: u8, arg: u64) -> [u8; REQUEST_LEN] {
    let mut body = [0u8; REQUEST_LEN];
    body[0] = op;
    body[1..].copy_from_slice(&arg.to_le_bytes());
    body
}

pub(crate) fn decode_request(body: &[u8]) -> Option<(u8, u64)> {
    let arg = body.get(1..REQUEST_LEN)?.try_into().ok()?;
    Some((*body.first()?, u64::from_le_bytes(arg)))
}
