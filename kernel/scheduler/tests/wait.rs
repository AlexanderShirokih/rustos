//! capability target-wait механизм: `block_current`/`unblock_thread` и состояние `Blocked`,
//! доступные через `KernelRuntime::block_current_until`/`unblock`. Проверяем
//! наблюдаемое поведение: уход current-потока в парк, корректное возобновление
//! и idempotency, timeout-парк через `SleepQueue`, а также stale-entry skip в
//! `wake_sleepers`.

mod common;

use std::sync::atomic::AtomicU32;

use capability::{KernelRuntime, ParkState, WaitToken};
use scheduler::{Scheduler, SchedulerConfig, SpawnConfig, Uninit};

use crate::common::{MockContext, MockTimer, MockTimerSource, reset_switches, with_simulated_irq};

type TestScheduler = Scheduler<MockContext, MockTimerSource, Uninit>;
const TEST_CONFIG: SchedulerConfig = SchedulerConfig::new(32, 16);

/// `ready_flag` в состоянии REGISTERED: park-сторона уйдёт в блокировку.
fn registered_flag() -> AtomicU32 {
    AtomicU32::new(ParkState::REGISTERED)
}

#[test]
fn block_current_parks_running_thread_and_switches_to_next() {
    reset_switches();
    let timer = MockTimer::new();
    let scheduler = TestScheduler::new(MockTimerSource(timer.clone()), TEST_CONFIG).bootstrap();
    let blocker = scheduler
        .spawn(SpawnConfig::new("blocker"), || {})
        .expect("spawn blocker");
    let runner = scheduler
        .spawn(SpawnConfig::new("runner"), || {})
        .expect("spawn runner");

    let running = scheduler.run();
    assert_eq!(running.current(), blocker);
    let handle = running.handle();

    let flag = registered_flag();
    handle.block_current_until(&flag, None);

    assert_eq!(running.current(), runner);
}

#[test]
fn blocked_thread_is_not_runnable_until_unblocked() {
    reset_switches();
    let timer = MockTimer::new();
    let scheduler = TestScheduler::new(MockTimerSource(timer.clone()), TEST_CONFIG).bootstrap();
    let blocker = scheduler
        .spawn(SpawnConfig::new("blocker"), || {})
        .expect("spawn blocker");
    let runner = scheduler
        .spawn(SpawnConfig::new("runner"), || {})
        .expect("spawn runner");

    let running = scheduler.run();
    assert_eq!(running.current(), blocker);
    let handle = running.handle();
    let blocker_token: WaitToken = handle.current_wait_token();

    let flag = registered_flag();
    handle.block_current_until(&flag, None);
    assert_eq!(running.current(), runner);

    // Пока `blocker` заблокирован, только `runner` остаётся runnable.
    running.yield_now();
    assert_eq!(running.current(), runner);

    handle.unblock(blocker_token);
    running.yield_now();
    assert_eq!(running.current(), blocker);
}

#[test]
fn unblock_is_idempotent_on_repeat_and_on_non_blocked_thread() {
    reset_switches();
    let timer = MockTimer::new();
    let scheduler = TestScheduler::new(MockTimerSource(timer.clone()), TEST_CONFIG).bootstrap();
    let blocker = scheduler
        .spawn(SpawnConfig::new("blocker"), || {})
        .expect("spawn blocker");
    let runner = scheduler
        .spawn(SpawnConfig::new("runner"), || {})
        .expect("spawn runner");

    let running = scheduler.run();
    assert_eq!(running.current(), blocker);
    let handle = running.handle();
    let blocker_token = handle.current_wait_token();

    handle.unblock(blocker_token);
    assert_eq!(running.current(), blocker);

    let flag = registered_flag();
    handle.block_current_until(&flag, None);
    assert_eq!(running.current(), runner);

    handle.unblock(blocker_token);
    // Повторный unblock не должен добавить второй экземпляр в ready-queue.
    handle.unblock(blocker_token);

    running.yield_now();
    assert_eq!(running.current(), blocker);
    // Если бы дубликат попал в очередь, blocker остался бы в ней повторно;
    // один полный round-robin должен вернуть на runner.
    running.yield_now();
    assert_eq!(running.current(), runner);
}

#[test]
fn block_with_timeout_wakes_via_sleep_queue_on_deadline() {
    reset_switches();
    let timer = MockTimer::new();
    let scheduler = TestScheduler::new(MockTimerSource(timer.clone()), TEST_CONFIG).bootstrap();
    let blocker = scheduler
        .spawn(SpawnConfig::new("blocker"), || {})
        .expect("spawn blocker");
    let runner = scheduler
        .spawn(SpawnConfig::new("runner"), || {})
        .expect("spawn runner");

    let running = scheduler.run();
    assert_eq!(running.current(), blocker);
    let handle = running.handle();

    let flag = registered_flag();
    handle.block_current_until(&flag, Some(50));
    assert_eq!(running.current(), runner);
    assert_eq!(timer.scheduled_deadline(), 50);

    with_simulated_irq(|| running.on_tick(10));
    assert_eq!(running.current(), runner);

    timer.advance_to(50);
    with_simulated_irq(|| running.on_tick(50));
    assert_eq!(running.current(), blocker);
}

#[test]
fn stale_sleep_entry_does_not_wake_thread_reparked_with_new_deadline() {
    // Сценарий из докстринга `block_current`: поток паркуется с timeout T1,
    // его будят раньше, затем он снова паркуется с другим дедлайном T2. Stale
    // SleepQueue-запись на T1 не должна разбудить поток на T1 - сравнение
    // `wakeup_at_ns` отсекает stale-entry. Будит только запись на T2.
    reset_switches();
    let timer = MockTimer::new();
    let scheduler = TestScheduler::new(MockTimerSource(timer.clone()), TEST_CONFIG).bootstrap();
    let blocker = scheduler
        .spawn(SpawnConfig::new("blocker"), || {})
        .expect("spawn blocker");
    let runner = scheduler
        .spawn(SpawnConfig::new("runner"), || {})
        .expect("spawn runner");

    let running = scheduler.run();
    assert_eq!(running.current(), blocker);
    let handle = running.handle();
    let blocker_token = handle.current_wait_token();

    // Парк #1: дедлайн T1 = 100. Stale-запись (blocker, 100) остаётся в
    // SleepQueue после ранней разблокировки.
    let flag1 = registered_flag();
    handle.block_current_until(&flag1, Some(100));
    assert_eq!(running.current(), runner);

    handle.unblock(blocker_token);
    running.yield_now();
    assert_eq!(running.current(), blocker);

    // Парк #2: новый дедлайн T2 = 50, wakeup_at_ns = 150.
    timer.advance_to(100);
    let flag2 = registered_flag();
    handle.block_current_until(&flag2, Some(50));
    assert_eq!(running.current(), runner);

    // На T1 (100) stale-entry (blocker, 100) пропускается: текущее
    // wakeup_at_ns == 150 != 100.
    timer.advance_to(100);
    with_simulated_irq(|| running.on_tick(100));
    running.yield_now();
    assert_eq!(
        running.current(),
        runner,
        "stale entry at T1 must not wake a thread parked until T2"
    );

    timer.advance_to(150);
    with_simulated_irq(|| running.on_tick(150));
    assert_eq!(running.current(), blocker);
}
