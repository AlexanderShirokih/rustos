//! Прогон выхода C++-программы с исключениями, `thread_local` и статическими объектами.
//!
//! Тестовый родитель запускает фикстуру без требований, как любого ребёнка, со своей стартовой
//! сессией и журналом. Строки фикстуры показывают, что после исключения и `thread_local`
//! доступны все `PTHREAD_KEYS_MAX` ключей, а деструкторы `thread_local` выполняются раньше
//! деструкторов статических объектов.

use managed_component::Outcome;

use crate::{log_relay::CXX_EXIT_BADGE, selftest_run::Stand};

const FIXTURE: Stand = Stand {
    entry: "/cxx-exit/exec",
    name: "cxx-exit",
    badge: CXX_EXIT_BADGE,
};

const LINES: [&str; 10] = [
    "global constructed",
    "first thread_local touched",
    "local static touched",
    "all keys created",
    "second thread_local touched",
    "main returns",
    "second thread_local destroyed",
    "first thread_local destroyed",
    "local static destroyed",
    "global destroyed",
];

#[test_harness::test]
fn cxx_program_leaves_thread_keys_free_and_destroys_thread_locals_before_statics() {
    let lines = FIXTURE.run(|child| child, Outcome::Exit(0), LINES.len());
    test_harness::check_eq!(lines, LINES);
}
