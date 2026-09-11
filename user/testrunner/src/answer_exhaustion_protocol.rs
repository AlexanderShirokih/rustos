//! Протокол фикстуры, у которой ответ с capability не умещается в таблицу handle, и коды её
//! выхода.

// Модуль подключён к обоим бинарям, и каждый пользуется своим подмножеством.
#![allow(dead_code)]

use ipc::wire::IpcError;
use syscall::kind;

/// Вызов получил ответ.
pub const ANSWERED_EXIT_CODE: u32 = 1;
/// Таблица handle не заполнилась до `OutOfHandles`.
pub const TABLE_NOT_FULL_EXIT_CODE: u32 = 2;
/// Вызов отказал с `IpcError::ResourceExhausted`.
pub const EXHAUSTED_EXIT_CODE: u32 = 3;
/// Вызов отказал с `IpcError::PeerClosed`.
pub const PEER_CLOSED_EXIT_CODE: u32 = 4;
/// Вызов отказал с иным `IpcError`.
pub const OTHER_ERROR_EXIT_CODE: u32 = 5;

ipc::protocol! {
    /// Контракт вызова, ответ на который несёт capability.
    pub mod answer_exhaustion {
        name = "AnswerExhaustion";

        /// Запрашивает capability ответом.
        call fn request() -> own ipc::wire::Cap<kind::Port>;
    }
}

/// Код выхода фикстуры, называющий отказ вызова.
pub fn exit_code_for(error: IpcError) -> u32 {
    match error {
        IpcError::ResourceExhausted => EXHAUSTED_EXIT_CODE,
        IpcError::PeerClosed => PEER_CLOSED_EXIT_CODE,
        _ => OTHER_ERROR_EXIT_CODE,
    }
}
