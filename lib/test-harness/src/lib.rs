//! Каркас интеграционных тестов, исполняемых в самой системе.
//!
//! Атрибут [`test`] помечает тестовую функцию. Макрос [`register_test!`] кладёт её
//! описание в секцию `.tests`. [`run_tests`] обходит эту секцию, исполняет тесты
//! выбранного вида [`TestKind`] и завершает процесс их итогом.
//!
//! Проверки внутри теста пишутся макросами [`check!`], [`check_eq!`] и [`check_ne!`],
//! отчёт - макросом [`report!`]. Тест без своего условия на этой машине, например без
//! описанного устройства, пропускается макросом [`skip!`]. Ход прогона размечен маркерами
//! `[TEST-RUN]`, `[TEST-START]`, `[TEST-PASS]`, `[TEST-SKIP]` и `[TEST-DONE]`, по которым
//! внешний наблюдатель разбирает результат.
//!
//! [`runner::install_writer`] ставит сток строк, [`runner::install_exit`] - способ завершения
//! процесса.

#![no_std]

#[cfg(test)]
extern crate std;

pub mod case;
pub mod macros;
pub mod runner;

pub use case::{TestCase, TestKind};
pub use runner::{run_all_tests, run_tests};
pub use test_harness_macros::test;
