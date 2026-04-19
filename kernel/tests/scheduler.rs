mod common;

use drivers_common::services::scheduler::{Priority, SpawnConfig};
use kernel::sched::{Scheduler, Uninit};

use crate::common::{MockContext, MockTimer, MockTimerSource, reset_switches};

type TestScheduler = Scheduler<MockContext, MockTimerSource, Uninit, 32, 1, 16>;

#[test]
fn run_starts_highest_priority_thread() {
    reset_switches();
    let timer = MockTimer::new();
    let scheduler = TestScheduler::new(MockTimerSource(timer.clone())).bootstrap();
    let low = scheduler
        .spawn(
            SpawnConfig::new("low").priority(Priority::new(8).expect("priority")),
            || {},
        )
        .expect("spawn low");
    let high = scheduler
        .spawn(
            SpawnConfig::new("high").priority(Priority::new(2).expect("priority")),
            || {},
        )
        .expect("spawn high");

    let running = scheduler.run();

    assert_eq!(running.current(), high);
    assert_ne!(low, high);
    assert_eq!(timer.scheduled_deadline(), 10_000_000);
}

#[test]
fn on_tick_round_robins_equal_priority_threads() {
    reset_switches();
    let timer = MockTimer::new();
    let scheduler = TestScheduler::new(MockTimerSource(timer.clone())).bootstrap();
    let first = scheduler
        .spawn(SpawnConfig::new("first"), || {})
        .expect("spawn first");
    let second = scheduler
        .spawn(SpawnConfig::new("second"), || {})
        .expect("spawn second");

    let running = scheduler.run();
    assert_eq!(running.current(), first);

    running.on_tick(0);
    assert_eq!(running.current(), second);

    running.on_tick(1);
    assert_eq!(running.current(), first);
}

#[test]
fn sleep_ns_blocks_and_wakes_thread_on_deadline() {
    reset_switches();
    let timer = MockTimer::new();
    let scheduler = TestScheduler::new(MockTimerSource(timer.clone())).bootstrap();
    let sleeper = scheduler
        .spawn(SpawnConfig::new("sleeper"), || {})
        .expect("spawn sleeper");
    let runner = scheduler
        .spawn(SpawnConfig::new("runner"), || {})
        .expect("spawn runner");

    let running = scheduler.run();
    assert_eq!(running.current(), sleeper);

    running.sleep_ns(50);
    assert_eq!(running.current(), runner);
    assert_eq!(timer.scheduled_deadline(), 50);

    timer.advance_to(50);
    running.on_tick(50);
    assert_eq!(running.current(), sleeper);
}
