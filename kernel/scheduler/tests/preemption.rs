//! Приоритетное вытеснение в `on_tick` (`scheduler.rs:789`,
//! `prio < current.priority()`). Базовые round-robin/time-slice кейсы живут в
//! `tests/scheduler.rs`; здесь проверяется именно выбор более приоритетного
//! потока при тике.

mod common;

use scheduler::{Priority, Scheduler, SchedulerConfig, SpawnConfig, Uninit};

use crate::common::{
    MockContext, MockTimer, MockTimerSource, reset_switches, switch_count, with_simulated_irq,
};

type TestScheduler = Scheduler<MockContext, MockTimerSource, Uninit>;
const TEST_CONFIG: SchedulerConfig = SchedulerConfig::new(32, 16);

#[test]
fn on_tick_preempts_to_higher_priority_thread() {
    use scheduler::SchedulerService;

    reset_switches();
    let timer = MockTimer::new();
    let scheduler = TestScheduler::new(MockTimerSource(timer.clone()), TEST_CONFIG).bootstrap();
    let low = scheduler
        .spawn(SpawnConfig::new("low").priority(Priority::new(20)), || {})
        .expect("spawn low");

    let running = scheduler.run();
    assert_eq!(running.current(), low);

    let high = running
        .handle()
        .spawn_boxed(
            SpawnConfig::new("high").priority(Priority::new(2)),
            Box::new(|| {}),
        )
        .expect("spawn high");

    let switches_before = switch_count();
    with_simulated_irq(|| running.on_tick(0));

    assert_eq!(running.current(), high);
    assert_eq!(switch_count(), switches_before + 1);
}

#[test]
fn on_tick_selects_highest_priority_among_several_ready() {
    use scheduler::SchedulerService;

    reset_switches();
    let timer = MockTimer::new();
    let scheduler = TestScheduler::new(MockTimerSource(timer.clone()), TEST_CONFIG).bootstrap();
    let low = scheduler
        .spawn(SpawnConfig::new("low").priority(Priority::new(20)), || {})
        .expect("spawn low");

    let running = scheduler.run();
    assert_eq!(running.current(), low);

    let handle = running.handle();
    let mid = handle
        .spawn_boxed(
            SpawnConfig::new("mid").priority(Priority::new(10)),
            Box::new(|| {}),
        )
        .expect("spawn mid");
    let high = handle
        .spawn_boxed(
            SpawnConfig::new("high").priority(Priority::new(3)),
            Box::new(|| {}),
        )
        .expect("spawn high");

    with_simulated_irq(|| running.on_tick(0));
    assert_eq!(
        running.current(),
        high,
        "scheduler must pick the highest-priority ready thread, not mid"
    );
    assert_ne!(running.current(), mid);
}
