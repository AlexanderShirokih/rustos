//! Прогон стенда стандартной библиотеки C++.
//!
//! Тестовый родитель запускает C++-программу без требований, как любого ребёнка, со своей
//! стартовой сессией и журналом. Стенд выводит в журнал начало и итог проверок и завершается
//! непойманным исключением, после которого обработчик завершения libc++abi пишет в журнал тип
//! и сообщение исключения и вызывает `abort`.

use managed_component::Outcome;

use crate::{log_relay::CXX_SELFTEST_BADGE, selftest_run::Stand};

const STAND: Stand = Stand {
    entry: "/cxx-selftest/exec",
    name: "cxx-selftest",
    badge: CXX_SELFTEST_BADGE,
};

const ABORT_EXIT_CODE: u32 = 134;

const LINES: [&str; 3] = [
    "selftest started",
    "checks passed",
    "libc++abi: terminating due to uncaught exception of type std::runtime_error: \
     selftest leaves main",
];

#[test_harness::test]
fn cxx_selftest_program_passes_its_checks_and_terminates_on_an_uncaught_exception() {
    // `abort` picolibc без обработчика `SIGABRT` завершает процесс кодом 128 + 6.
    let lines = STAND.run(|child| child, Outcome::Exit(ABORT_EXIT_CODE), LINES.len());
    test_harness::check_eq!(lines, LINES);
}
