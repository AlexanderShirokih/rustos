//! Коды выхода `two-requirements-fixture` и `helper-panic-fixture`, которые сверяют тесты
//! `startup_port`.

// Модуль подключён к прогону и к фикстурам, и каждый бинарь пользуется своим подмножеством.
#![allow(dead_code)]

/// Провайдер отклонил сессию второго требования.
pub const SECOND_SESSION_REFUSED_EXIT_CODE: u32 = 2;
/// Запрос первого требования не прошёл по IPC.
pub const FIRST_IPC_FAILED_EXIT_CODE: u32 = 3;
/// Вспомогательный поток запаниковал.
pub const PANIC_EXIT_CODE: u32 = 8;
