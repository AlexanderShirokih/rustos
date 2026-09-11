//! Прогон стенда стандартной библиотеки.
//!
//! Тестовый родитель запускает программу на `std`, как любого ребёнка, со своей стартовой
//! сессией и журналом. Программа сообщает итог проверок кодом выхода, а подробности провала
//! уходят в журнал сообщением паники.

use kernel_tests::kernel_test;
use managed_component::{Departure, Outcome};
use runtime::Timeout;

use crate::{
    fixture::{process_bytes, program},
    log_relay::{SELFTEST_BADGE, component_log_session},
    parent::{Answer, Child, Parent},
};

const SELFTEST_ENTRY: &str = "/boot/std-selftest/program";

const INSTANCE_NAME: &str = "std-selftest";

const PROCESS_TIMEOUT: Timeout = Timeout::from_secs(10);

const HEAP_SLACK_BYTES: u64 = 4 * 1024 * 1024;

#[kernel_test]
fn std_selftest_program_passes_its_checks() {
    let log = component_log_session(SELFTEST_BADGE).expect("selftest log session");
    let budget = process_bytes(&program(SELFTEST_ENTRY)) + HEAP_SLACK_BYTES;
    let parent = Parent::start();

    let selftest = parent.add(
        Child::new(SELFTEST_ENTRY, INSTANCE_NAME)
            .log(Answer::log(log))
            .budget(budget),
    );
    kernel_tests::kassert_eq!(
        parent.departure(PROCESS_TIMEOUT),
        Some(Departure {
            child: selftest,
            outcome: Outcome::Exit(0),
        })
    );
}
