//! Прогон стенда стандартной библиотеки.
//!
//! Тестовый родитель запускает программу на `std`, как любого ребёнка, со своей стартовой
//! сессией и журналом. Программа сообщает итог проверок кодом выхода, а подробности провала
//! уходят в журнал сообщением паники.

use alloc::string::String;

use filesystem_abi::namespace;
use ipc::Protocol as _;
use managed_component::{Departure, Outcome};
use rootkeeper::relay::LogSessions as _;
use runtime::Timeout;

use crate::{
    failing_filesystem::mount_failing,
    filesystem::root_view,
    fixture::{process_bytes, program},
    log_relay::{
        SELFTEST_BADGE, ScenarioLogSessions, SharedSourceNames, await_component_lines,
        start_component_relay, stop_component_relay,
    },
    parent::{Answer, Child, Parent},
};

const SELFTEST_ENTRY: &str = "/std-selftest/exec";

const INSTANCE_NAME: &str = "std-selftest";

const NAMESPACE_REQUIREMENT: &str = "namespace";

const PROCESS_TIMEOUT: Timeout = Timeout::from_secs(20);

const HEAP_SLACK_BYTES: u64 = 4 * 1024 * 1024;

#[test_harness::test]
fn std_selftest_program_passes_its_checks() {
    let failing = mount_failing();

    start_component_relay(SharedSourceNames);
    let log = ScenarioLogSessions
        .session(SELFTEST_BADGE)
        .expect("selftest log session");
    let budget = process_bytes(&program(SELFTEST_ENTRY)) + HEAP_SLACK_BYTES;
    let parent = Parent::start();

    let selftest = parent.add(
        Child::new(SELFTEST_ENTRY, INSTANCE_NAME)
            .log(Answer::log(log))
            .requirement(
                NAMESPACE_REQUIREMENT,
                Answer::Transfer {
                    protocol: namespace::Protocol::ID,
                    source: Some(root_view()),
                },
            )
            .budget(budget),
    );

    test_harness::check_eq!(
        parent.departure(PROCESS_TIMEOUT),
        Some(Departure {
            child: selftest,
            outcome: Outcome::Exit(0),
        })
    );

    let lines = await_component_lines(INSTANCE_NAME, 3);
    stop_component_relay();
    test_harness::check_eq!(lines.first().map(String::as_str), Some("selftest started"));
    test_harness::check_eq!(lines.last().map(String::as_str), Some("checks passed"));

    drop(failing);
}
