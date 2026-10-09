//! Прогон стенда библиотеки C.
//!
//! Тестовый родитель запускает C-программу, как любого ребёнка, со своей стартовой сессией,
//! журналом, корнем пространства имён, под которым смонтирована отказывающая файловая система, и
//! портом `Rtc`, на котором поток теста отвечает заданным временем. Стенд сообщает итог проверок
//! кодом выхода, время суток после установки часов - строкой `time N`, а путь `exit` - маркерами
//! обработчиков `atexit` в обратном порядке регистрации и последней строкой вывода, которую
//! выталкивает завершение процесса. Строки, которые потоки стенда пишут одновременно, приходят
//! раньше маркеров в любом порядке, но каждая целиком. Запас кучи сверх образа меньше запроса,
//! которым стенд проверяет отказ `malloc` и `pthread_create`.

use alloc::{format, string::String, vec::Vec};

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
        C_SELFTEST_BADGE, ScenarioLogSessions, SharedSourceNames, await_component_lines,
        start_component_relay, stop_component_relay,
    },
    parent::{Answer, Child, Parent},
    selftest_clock::{RTC_REQUIREMENT, SelftestClock, printed_time},
};

const SELFTEST_ENTRY: &str = "/c-selftest/exec";

const INSTANCE_NAME: &str = "c-selftest";

const NAMESPACE_REQUIREMENT: &str = "namespace";

const PROCESS_TIMEOUT: Timeout = Timeout::from_secs(20);

const HEAP_SLACK_BYTES: u64 = 4 * 1024 * 1024;

const PASSED_EXIT_CODE: u32 = 3;

const EXIT_LINES: [&str; 3] = ["atexit B", "atexit A", "exit without newline"];

const PRINTING_THREADS: usize = 4;

const PRINTED_LINES: usize = 8;

#[test_harness::test]
fn c_selftest_program_passes_its_checks_and_exits_through_atexit() {
    let failing = mount_failing();

    start_component_relay(SharedSourceNames);
    let log = ScenarioLogSessions
        .session(C_SELFTEST_BADGE)
        .expect("selftest log session");
    let budget = process_bytes(&program(SELFTEST_ENTRY)) + HEAP_SLACK_BYTES;
    let clock = SelftestClock::start(PROCESS_TIMEOUT);
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
            .requirement(RTC_REQUIREMENT, clock.answer())
            .budget(budget),
    );

    test_harness::check_eq!(
        parent.departure(PROCESS_TIMEOUT),
        Some(Departure {
            child: selftest,
            outcome: Outcome::Exit(PASSED_EXIT_CODE),
        })
    );
    let times = clock.finish();

    let mut expected: Vec<String> = (0..PRINTING_THREADS)
        .flat_map(|thread| {
            (0..PRINTED_LINES).map(move |line| format!("thread {thread} line {line}"))
        })
        .collect();
    let mut lines = await_component_lines(INSTANCE_NAME, expected.len() + 1 + EXIT_LINES.len());
    stop_component_relay();
    let mut tail = lines.split_off(expected.len().min(lines.len())).into_iter();
    lines.sort();
    expected.sort();
    test_harness::check_eq!(lines, expected);
    let time = tail.next().as_deref().and_then(printed_time);
    test_harness::check!(time.is_some_and(|time| times.contains(&time)));
    let exit_lines: Vec<String> = tail.collect();
    let exit_lines: Vec<&str> = exit_lines.iter().map(String::as_str).collect();
    test_harness::check_eq!(exit_lines, EXIT_LINES);

    drop(failing);
}
