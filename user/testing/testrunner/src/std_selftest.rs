//! Прогон стенда стандартной библиотеки.
//!
//! Тестовый родитель запускает программу на `std`, как любого ребёнка, со своей стартовой
//! сессией, журналом и портом `Rtc`, на котором поток теста отвечает заданным временем.
//! Программа сообщает итог проверок кодом выхода, время суток после установки часов - строкой
//! `time N`, а подробности провала уходят в журнал сообщением паники.

use alloc::string::String;

use filesystem_abi::namespace;
use ipc::Protocol as _;
use managed_component::Outcome;

use crate::{
    failing_filesystem::mount_failing,
    filesystem::root_view,
    log_relay::SELFTEST_BADGE,
    parent::Answer,
    selftest_clock::{RTC_REQUIREMENT, SelftestClock, printed_time},
    selftest_run::{PROCESS_TIMEOUT, Stand},
};

const STAND: Stand = Stand {
    entry: "/std-selftest/exec",
    name: "std-selftest",
    badge: SELFTEST_BADGE,
    timeout: PROCESS_TIMEOUT,
};

const NAMESPACE_REQUIREMENT: &str = "namespace";

#[test_harness::test]
fn std_selftest_program_passes_its_checks() {
    let failing = mount_failing();
    let clock = SelftestClock::start(STAND.timeout);

    let lines = STAND.run(
        |child| {
            child
                .requirement(
                    NAMESPACE_REQUIREMENT,
                    Answer::Transfer {
                        protocol: namespace::Protocol::ID,
                        source: Some(root_view()),
                    },
                )
                .requirement(RTC_REQUIREMENT, clock.answer())
        },
        Outcome::Exit(0),
        4,
    );

    let times = clock.finish();
    test_harness::check_eq!(lines.first().map(String::as_str), Some("selftest started"));
    test_harness::check_eq!(lines.last().map(String::as_str), Some("checks passed"));
    let time = lines.iter().find_map(|line| printed_time(line));
    test_harness::check!(time.is_some_and(|time| times.contains(&time)));

    drop(failing);
}
