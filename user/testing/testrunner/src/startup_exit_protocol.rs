//! Коды выхода `two-requirements-fixture`, которые сверяют тесты `startup_port`.

// Модуль подключён к прогону и к фикстуре, и каждый бинарь пользуется своим подмножеством.
#![allow(dead_code)]

pub(crate) const SECOND_SESSION_REFUSED_EXIT_CODE: u32 = 2;
pub(crate) const FIRST_IPC_FAILED_EXIT_CODE: u32 = 3;
