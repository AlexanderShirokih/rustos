//! Дескриптор схемы протокола.
//!
//! Машинно-читаемое описание контракта (операции, поля, ordinal),
//! достаточное для генерации совместимого клиента без чтения исходника.
//! Единый источник истины для `ipc-macros` (compile-time) и `ipc` (runtime).

#![no_std]

/// Размер заголовка кадра в байтах (`ordinal` + `txid` + `flags`).
pub const HEADER_SIZE: usize = 14;

/// Накладные расходы одной записи поля: `field_id` (1 байт) + длина (`u16` LE).
pub const FIELD_OVERHEAD: usize = 3;

/// Вид операции протокола.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Запрос-ответ клиент->сервер.
    Call,
    /// Односторонняя операция клиент->сервер без ответа.
    Cast,
    /// Односторонняя операция сервер->клиент без ответа.
    Event,
}

/// Тег типа значения в сообщении.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WireType {
    /// Беззнаковое целое заданной ширины в байтах.
    Uint(u8),
    /// Знаковое целое заданной ширины в байтах.
    Int(u8),
    /// Логическое значение (один байт).
    Bool,
    /// Bounded UTF-8 строка с границей длины `N`.
    BoundedStr(usize),
    /// Bounded байтовая строка с границей длины `N`.
    BoundedBytes(usize),
    /// Capability: handle едет вне тела, в поле - индекс в handle-массиве.
    Capability,
    /// Агрегат: поля в порядке объявления, каждое - вложенный `WireType`.
    Aggregate(&'static [WireType]),
}

/// Описание одного поля операции: позиционный `field_id` и тип значения.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FieldDesc {
    /// Позиционный идентификатор поля (`1..=254`).
    pub field_id: u8,
    /// Тип значения поля в сообщении.
    pub field_type: WireType,
}

/// Описание одной операции протокола.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OperationDesc {
    /// Имя операции (без префикса протокола).
    pub name: &'static str,
    /// Каноническое имя операции (`<protocol>.<operation>`).
    pub canonical: &'static str,
    /// Идентификатор операции (хэш канонического имени).
    pub ordinal: u64,
    /// Вид операции.
    pub kind: Kind,
    /// Поля параметров операции в порядке объявления.
    pub fields: &'static [FieldDesc],
}

/// Дескриптор протокола: список операций.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProtocolDesc {
    /// Операции протокола в порядке объявления.
    pub operations: &'static [OperationDesc],
}
