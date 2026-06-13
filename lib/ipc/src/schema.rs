//! Дескриптор схемы протокола.
//!
//! Машинно-читаемое описание контракта (операции, поля, ordinal),
//! достаточное для генерации совместимого клиента без чтения исходника.
//! Типы определены в `ipc-schema` и общие с `ipc-macros`.

pub use ipc_schema::{FieldDesc, Kind, OperationDesc, ProtocolDesc, WireType};
