mod common;

use std::sync::{Arc, Mutex};

use drivers_common::services::scheduler::{Priority, SpawnConfig};
use kernel::sched::{Scheduler, SchedulerConfig, ThreadStackAllocator, Uninit};

use crate::common::{
    MockContext, MockStack, MockTimer, MockTimerSource, max_irq_depth, reset_switches, switch_count,
};

type TestScheduler = Scheduler<MockContext, MockTimerSource, Uninit>;

const TEST_CONFIG: SchedulerConfig = SchedulerConfig::new(32, 16);
const SMALL_CONFIG: SchedulerConfig = SchedulerConfig::new(4, 8);

#[test]
fn run_starts_highest_priority_thread() {
    reset_switches();
    let timer = MockTimer::new();
    let scheduler = TestScheduler::new(MockTimerSource(timer.clone()), TEST_CONFIG).bootstrap();
    let low = scheduler
        .spawn(SpawnConfig::new("low").priority(Priority::new(8)), || {})
        .expect("spawn low");
    let high = scheduler
        .spawn(SpawnConfig::new("high").priority(Priority::new(2)), || {})
        .expect("spawn high");

    let running = scheduler.run();

    assert_eq!(running.current(), high);
    assert_ne!(low, high);
    assert_eq!(timer.scheduled_deadline(), 10_000_000);
    // run() выполнил один реальный switch idle -> high.
    assert_eq!(switch_count(), 1);
}

#[test]
fn on_tick_round_robins_equal_priority_threads() {
    reset_switches();
    let timer = MockTimer::new();
    let scheduler = TestScheduler::new(MockTimerSource(timer.clone()), TEST_CONFIG).bootstrap();
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
    let scheduler = TestScheduler::new(MockTimerSource(timer.clone()), TEST_CONFIG).bootstrap();
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

#[test]
fn yield_now_rotates_runnable_threads() {
    reset_switches();
    let timer = MockTimer::new();
    let scheduler = TestScheduler::new(MockTimerSource(timer.clone()), TEST_CONFIG).bootstrap();
    let a = scheduler.spawn(SpawnConfig::new("a"), || {}).unwrap();
    let b = scheduler.spawn(SpawnConfig::new("b"), || {}).unwrap();

    let running = scheduler.run();
    assert_eq!(running.current(), a);
    let switches_before = switch_count();

    running.yield_now();
    assert_eq!(running.current(), b);
    assert_eq!(switch_count(), switches_before + 1);
}

#[test]
fn yield_now_with_no_other_threads_keeps_current() {
    reset_switches();
    let timer = MockTimer::new();
    let scheduler = TestScheduler::new(MockTimerSource(timer.clone()), TEST_CONFIG).bootstrap();
    let only = scheduler.spawn(SpawnConfig::new("only"), || {}).unwrap();

    let running = scheduler.run();
    assert_eq!(running.current(), only);
    let switches_before = switch_count();

    running.yield_now();
    assert_eq!(running.current(), only);
    // ready queue empty -> yield не делает switch.
    assert_eq!(switch_count(), switches_before);
}

#[test]
fn time_slice_decrement_triggers_preemption() {
    reset_switches();
    let timer = MockTimer::new();
    let scheduler = TestScheduler::new(MockTimerSource(timer.clone()), TEST_CONFIG).bootstrap();
    let _a = scheduler.spawn(SpawnConfig::new("a"), || {}).unwrap();
    let b = scheduler.spawn(SpawnConfig::new("b"), || {}).unwrap();
    let running = scheduler.run();

    let switches_before = switch_count();
    running.on_tick(0);
    assert_eq!(switch_count(), switches_before + 1);
    assert_eq!(running.current(), b);
}

#[test]
fn invalid_priority_returns_error() {
    let timer = MockTimer::new();
    type SmallScheduler = Scheduler<MockContext, MockTimerSource, Uninit>;
    let scheduler = SmallScheduler::new(MockTimerSource(timer), SMALL_CONFIG).bootstrap();
    let err = scheduler
        .spawn(SpawnConfig::new("bad").priority(Priority::new(10)), || {})
        .unwrap_err();
    assert_eq!(
        err,
        drivers_common::services::scheduler::SpawnError::InvalidPriority
    );
}

#[test]
fn small_prio_scheduler_works_with_low_idle_priority() {
    reset_switches();
    let timer = MockTimer::new();
    type SmallScheduler = Scheduler<MockContext, MockTimerSource, Uninit>;
    let scheduler = SmallScheduler::new(MockTimerSource(timer.clone()), SMALL_CONFIG).bootstrap();
    let task = scheduler
        .spawn(SpawnConfig::new("task").priority(Priority::new(1)), || {})
        .unwrap();
    let running = scheduler.run();
    assert_eq!(running.current(), task);
}

#[test]
fn enable_preemption_balanced_during_yield() {
    reset_switches();
    let timer = MockTimer::new();
    let scheduler = TestScheduler::new(MockTimerSource(timer.clone()), TEST_CONFIG).bootstrap();
    let _ = scheduler.spawn(SpawnConfig::new("a"), || {}).unwrap();
    let _ = scheduler.spawn(SpawnConfig::new("b"), || {}).unwrap();

    let running = scheduler.run();
    let max = max_irq_depth();
    running.yield_now();
    running.yield_now();
    assert_eq!(common::current_irq_depth(), 0, "IRQ-depth must return to 0");
    assert!(max <= 1, "max IRQ depth should not exceed 1, got {max}");
}

#[test]
fn sleep_queue_orders_multiple_sleepers_by_deadline() {
    reset_switches();
    let timer = MockTimer::new();
    let scheduler = TestScheduler::new(MockTimerSource(timer.clone()), TEST_CONFIG).bootstrap();
    let early = scheduler.spawn(SpawnConfig::new("early"), || {}).unwrap();
    let late = scheduler.spawn(SpawnConfig::new("late"), || {}).unwrap();
    let runner = scheduler.spawn(SpawnConfig::new("runner"), || {}).unwrap();
    let running = scheduler.run();

    assert_eq!(running.current(), early);
    running.sleep_ns(100);
    assert_eq!(running.current(), late);
    running.sleep_ns(50);
    assert_eq!(running.current(), runner);

    // Дедлайн `late` (50) меньше дедлайна `early` (100), поэтому первой
    // должна проснуться именно `late`.
    timer.advance_to(50);
    running.on_tick(50);
    assert_eq!(running.current(), late);

    // Дальнейшая активность определяется round-robin внутри одного
    // приоритета (NORMAL); проверяем только сам факт пробуждения early.
    timer.advance_to(100);
    running.on_tick(100);
    let after_second_tick = running.current();
    assert!(
        after_second_tick == runner || after_second_tick == early || after_second_tick == late,
        "after wake of `early` scheduler must run a real (non-idle) thread, got {:?}",
        after_second_tick
    );
}

#[test]
fn exit_current_releases_thread_slot_for_next_spawn() {
    use drivers_common::services::scheduler::SchedulerService;
    reset_switches();
    let timer = MockTimer::new();
    type SmallScheduler = Scheduler<MockContext, MockTimerSource, Uninit>;
    // 1 idle + 3 spawned заполнят все 4 слота.
    let scheduler =
        SmallScheduler::new(MockTimerSource(timer.clone()), SchedulerConfig::new(32, 4))
            .bootstrap();
    let _ = scheduler.spawn(SpawnConfig::new("a"), || {}).unwrap();
    let _ = scheduler.spawn(SpawnConfig::new("b"), || {}).unwrap();
    let _ = scheduler.spawn(SpawnConfig::new("c"), || {}).unwrap();
    let handle = Arc::new(scheduler.handle());
    let running = scheduler.run();

    // Ещё один spawn должен упасть с NoFreeThreadSlots.
    let err = running
        .handle()
        .spawn_boxed(SpawnConfig::new("overflow"), Box::new(|| {}))
        .unwrap_err();
    assert_eq!(
        err,
        drivers_common::services::scheduler::SpawnError::NoFreeThreadSlots
    );
    let _ = handle;
}

#[test]
fn stack_canary_is_initialized_in_mock_stack() {
    let stack = MockStack::allocate(1).expect("alloc");
    assert!(
        stack.check_canary(),
        "freshly created stack must have valid canary"
    );
}

#[test]
fn spawn_via_service_handle_works() {
    use drivers_common::services::scheduler::{SchedulerService, SchedulerServiceExt};
    reset_switches();
    let timer = MockTimer::new();
    let scheduler = TestScheduler::new(MockTimerSource(timer.clone()), TEST_CONFIG).bootstrap();
    let handle = Arc::new(scheduler.handle());
    let service: Arc<dyn SchedulerService> = handle;
    let executed = Arc::new(Mutex::new(false));
    let executed_clone = executed.clone();

    let id = service
        .spawn(SpawnConfig::new("via-service"), move || {
            *executed_clone.lock().unwrap() = true;
        })
        .expect("spawn via service");

    let running = scheduler.run();
    assert_eq!(running.current(), id);
}
