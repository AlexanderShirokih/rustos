use managed_component::Outcome;

use crate::{
    log_relay::SLINT_SELFTEST_BADGE,
    selftest_run::{PROCESS_TIMEOUT, Stand},
};

const STAND: Stand = Stand {
    entry: "/slint-selftest/exec",
    name: "slint-selftest",
    badge: SLINT_SELFTEST_BADGE,
    timeout: PROCESS_TIMEOUT,
};

#[test_harness::test]
fn slint_delivers_input_during_present_and_wakes_for_tasks_and_timers() {
    let lines = STAND.run(|child| child, Outcome::Exit(0), 1);
    test_harness::check_eq!(lines, ["slint checks passed"]);
}
