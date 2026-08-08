//! Протокол `port-fixture`: запрос-ответ поверх bootstrap-порта потомка.
//!
//! Запрос - опкод и один `u64`-аргумент; ответ зависит от опкода. Родитель
//! всегда идёт через `call`, поэтому у фикстуры на каждый запрос есть
//! reply-хэндл.

// Модуль подключён к обоим бинарям: родитель кодирует запросы и читает коды
// ответов, фикстура - декодирует.
#![allow(dead_code)]

/// Записать аргумент в слот фикстуры. Ответ - [`REPLY_OK`].
pub const OP_WRITE_SLOT: u8 = 1;
/// Прочитать слот. Ответ - значение и адрес слота, [`READ_REPLY_LEN`] байт.
pub const OP_READ_SLOT: u8 = 2;
/// Замапить регион из `caps[0]`, сверить первые 8 байт с аргументом и
/// записать туда его инверсию. Ответ - [`REPLY_OK`] либо код расхождения.
pub const OP_MAP_CHECK: u8 = 3;
/// Ответить [`REPLY_OK`] и завершиться с кодом из аргумента.
pub const OP_EXIT: u8 = 4;
/// Выделить регион на аргумент страниц из собственного метеринг-ресурса и
/// сразу закрыть его. Ответ - [`REPLY_OK`] либо код отказа.
pub const OP_ALLOCATE_PAGES: u8 = 5;
/// Снять с собственного метеринг-ресурса капу с правом `TRANSFER`. Ответ -
/// [`REPLY_OK`], [`REPLY_ACCESS_DENIED`] либо [`REPLY_DUPLICATE_FAILED`].
pub const OP_DUPLICATE_TRANSFER_CAP: u8 = 6;
/// Поднять поток, паркующийся бессрочно, ответить [`REPLY_OK`] либо
/// [`REPLY_SPAWN_FAILED`] и завершить процесс целиком с кодом из аргумента.
pub const OP_PROCESS_EXIT: u8 = 7;

/// Длина запроса: опкод и `u64`-аргумент.
pub const REQUEST_LEN: usize = 1 + 8;
/// Длина ответа `OP_READ_SLOT`: значение слота и его адрес.
pub const READ_REPLY_LEN: usize = 8 + 8;

/// Успех односложного ответа.
pub const REPLY_OK: u8 = 0;
/// `OP_MAP_CHECK`: прочитанное из региона не совпало с ожидаемым.
pub const REPLY_PATTERN_MISMATCH: u8 = 1;
/// `OP_MAP_CHECK`: регион не удалось замапить.
pub const REPLY_MAP_FAILED: u8 = 2;
/// `OP_ALLOCATE_PAGES`: бюджет метеринг-ресурса фикстуры исчерпан.
pub const REPLY_BUDGET_EXHAUSTED: u8 = 3;
/// `OP_ALLOCATE_PAGES`: выделение отказано не бюджетом.
pub const REPLY_ALLOCATE_FAILED: u8 = 4;
/// `OP_DUPLICATE_TRANSFER_CAP`: право `TRANSFER` капе фикстуры не выдано.
pub const REPLY_ACCESS_DENIED: u8 = 5;
/// `OP_DUPLICATE_TRANSFER_CAP`: дубликат отказан не правами.
pub const REPLY_DUPLICATE_FAILED: u8 = 6;
/// `OP_PROCESS_EXIT`: паркующийся поток не поднялся.
pub const REPLY_SPAWN_FAILED: u8 = 7;

/// Сериализует запрос в кадр фиксированной длины.
pub fn encode_request(op: u8, arg: u64) -> [u8; REQUEST_LEN] {
    let mut body = [0u8; REQUEST_LEN];
    body[0] = op;
    body[1..].copy_from_slice(&arg.to_le_bytes());
    body
}

/// Разбирает запрос; `None` - кадр короче [`REQUEST_LEN`].
pub fn decode_request(body: &[u8]) -> Option<(u8, u64)> {
    let arg = body.get(1..REQUEST_LEN)?.try_into().ok()?;
    Some((*body.first()?, u64::from_le_bytes(arg)))
}
