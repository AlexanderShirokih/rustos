//! Прогон стенда sqlite.
//!
//! Тестовый родитель запускает C-программу, как любого ребёнка, со своей стартовой сессией,
//! журналом и корнем пространства имён, где `/tmp` - файловая система в памяти. Стенд проверяет
//! сценарии спецификации sqlite над базой на `/tmp` и сообщает итог кодом выхода. Запросы, их
//! результаты и имя проваленной проверки стенд пишет в журнал.

use filesystem_abi::namespace;
use ipc::Protocol as _;
use managed_component::Outcome;

use crate::{
    filesystem::root_view, log_relay::SQLITE_SELFTEST_BADGE, parent::Answer, selftest_run::Stand,
};

const STAND: Stand = Stand {
    entry: "/sqlite-selftest/exec",
    name: "sqlite-selftest",
    badge: SQLITE_SELFTEST_BADGE,
};

const NAMESPACE_REQUIREMENT: &str = "namespace";

#[test_harness::test]
fn sqlite_selftest_program_passes_its_checks() {
    STAND.run(
        |child| {
            child.requirement(
                NAMESPACE_REQUIREMENT,
                Answer::Transfer {
                    protocol: namespace::Protocol::ID,
                    source: Some(root_view()),
                },
            )
        },
        Outcome::Exit(0),
        0,
    );
}
