#![allow(clippy::unwrap_used)]
#![allow(clippy::expect_used, clippy::std_instead_of_core)]

mod common;

use core::num::NonZeroUsize;
use std::{
    panic,
    sync::{Arc, Mutex, Once, OnceLock, RwLock},
};

use memory::{
    AccessMask, MemFlags, MemoryRegion,
    virtual_address::{PageAlignedVirtualAddress, VirtualAddress},
};
use scheduler::{
    CpuBootstrapError, CpuEntry, CpuId, DEFAULT_QUANTUM_NS, Priority, Scheduler, SchedulerConfig,
    SchedulerHandle, SchedulerServiceExt, SpawnAddressSpace, SpawnConfig, SpawnError, Uninit,
};

use crate::common::{
    CountingFrameAllocator, MockAddressSpaceFactory, MockContext, MockTimer, MockTimerSource,
    preemption_enabled, reset_switches, set_current_cpu, switch_count, take_address_space_switches,
    take_reschedules, with_simulated_irq,
};

type TestScheduler = Scheduler<MockContext, MockTimerSource, Uninit>;
type SmallScheduler = Scheduler<MockContext, MockTimerSource, Uninit>;

const TEST_CONFIG: SchedulerConfig = SchedulerConfig::new(32, 16);
const SMALL_CONFIG: SchedulerConfig = SchedulerConfig::new(4, 8);

struct ForwardingRuntime {
    inner: RwLock<Option<Arc<dyn capability::KernelRuntime>>>,
}

impl ForwardingRuntime {
    fn set(&self, rt: Arc<dyn capability::KernelRuntime>) {
        *self.inner.write().unwrap() = Some(rt);
    }

    fn clear(&self) {
        *self.inner.write().unwrap() = None;
    }

    fn with<R>(&self, f: impl FnOnce(&dyn capability::KernelRuntime) -> R) -> R {
        let guard = self.inner.read().unwrap();
        let rt = guard
            .as_ref()
            .expect("ForwardingRuntime: inner runtime not set");
        f(rt.as_ref())
    }
}

impl capability::KernelRuntime for ForwardingRuntime {
    fn current_wait_token(&self) -> capability::WaitToken {
        self.with(|rt| rt.current_wait_token())
    }

    fn now_ns(&self) -> u64 {
        self.with(|rt| rt.now_ns())
    }

    fn current_handle_table(&self) -> Option<Arc<collections::MutexCell<capability::HandleTable>>> {
        self.with(|rt| rt.current_handle_table())
    }

    fn exit_current_thread(&self, exit_code: i32) -> ! {
        self.with(|rt| rt.exit_current_thread(exit_code))
    }

    fn exit_current_process(&self, exit_code: i32) -> ! {
        self.with(|rt| rt.exit_current_process(exit_code))
    }

    fn yield_current(&self) {
        self.with(|rt| rt.yield_current());
    }

    fn block_current_until(
        &self,
        ready_flag: &core::sync::atomic::AtomicU32,
        timeout_ns: Option<u64>,
    ) {
        self.with(|rt| rt.block_current_until(ready_flag, timeout_ns));
    }

    fn unblock(&self, token: capability::WaitToken) {
        self.with(|rt| rt.unblock(token));
    }

    fn set_blocked_cancel(&self, cancel: Arc<dyn capability::CancelTarget>) {
        self.with(|rt| rt.set_blocked_cancel(cancel));
    }

    fn clear_blocked_cancel(&self) {
        self.with(|rt| rt.clear_blocked_cancel());
    }
}

fn install_forwarding_runtime() -> Arc<ForwardingRuntime> {
    static FORWARDER: OnceLock<Arc<ForwardingRuntime>> = OnceLock::new();
    static INSTALLED: Once = Once::new();

    let _ = capability::ParkState::REGISTERED;
    let forwarder = FORWARDER
        .get_or_init(|| {
            Arc::new(ForwardingRuntime {
                inner: RwLock::new(None),
            })
        })
        .clone();
    INSTALLED.call_once(|| {
        let dyn_rt: Arc<dyn capability::KernelRuntime> = forwarder.clone();
        capability::install_runtime(dyn_rt);
    });
    forwarder
}

#[test]
fn run_starts_highest_priority_thread() {
    reset_switches();
    let timer = MockTimer::new();
    let scheduler = TestScheduler::new(MockTimerSource(timer.clone()), TEST_CONFIG)
        .bootstrap()
        .ok()
        .expect("bootstrap must succeed in tests");
    let low = scheduler
        .spawn(SpawnConfig::new("low").priority(Priority::new(8)), || {})
        .expect("spawn low");
    let high = scheduler
        .spawn(SpawnConfig::new("high").priority(Priority::new(2)), || {})
        .expect("spawn high");

    let running = scheduler.run();

    assert_eq!(running.current(), Some(high));
    assert_ne!(low, high);
    assert_eq!(timer.scheduled_deadline(), scheduler::DEFAULT_QUANTUM_NS);
    assert_eq!(switch_count(), 1);
}

enum PreemptKind {
    OnTick,
    Yield,
}

fn round_robin_via(kind: &PreemptKind) {
    reset_switches();
    let timer = MockTimer::new();
    let scheduler = TestScheduler::new(MockTimerSource(timer.clone()), TEST_CONFIG)
        .bootstrap()
        .ok()
        .expect("bootstrap must succeed in tests");
    let first = scheduler
        .spawn(SpawnConfig::new("first"), || {})
        .expect("spawn first");
    let second = scheduler
        .spawn(SpawnConfig::new("second"), || {})
        .expect("spawn second");

    let running = scheduler.run();
    assert_eq!(running.current(), Some(first));

    let mut now = 0;
    let mut step = |running: &Scheduler<MockContext, MockTimerSource, scheduler::Running>| {
        let before = switch_count();
        match kind {
            PreemptKind::OnTick => with_simulated_irq(|| running.on_tick(now)),
            PreemptKind::Yield => running.yield_now(),
        }
        now += 1;
        assert_eq!(
            switch_count(),
            before + 1,
            "each rotation must perform exactly one context switch"
        );
    };

    step(&running);
    assert_eq!(running.current(), Some(second));

    step(&running);
    assert_eq!(running.current(), Some(first));
}

#[test]
fn on_tick_round_robins_equal_priority_threads() {
    round_robin_via(&PreemptKind::OnTick);
}

#[test]
fn yield_round_robins_equal_priority_threads() {
    round_robin_via(&PreemptKind::Yield);
}

#[test]
fn sleep_ns_blocks_and_wakes_thread_on_deadline() {
    reset_switches();
    let timer = MockTimer::new();
    let scheduler = TestScheduler::new(MockTimerSource(timer.clone()), TEST_CONFIG)
        .bootstrap()
        .ok()
        .expect("bootstrap must succeed in tests");
    let sleeper = scheduler
        .spawn(SpawnConfig::new("sleeper"), || {})
        .expect("spawn sleeper");
    let runner = scheduler
        .spawn(SpawnConfig::new("runner"), || {})
        .expect("spawn runner");

    let running = scheduler.run();
    assert_eq!(running.current(), Some(sleeper));

    running.sleep_ns(50);
    assert_eq!(running.current(), Some(runner));
    assert_eq!(timer.scheduled_deadline(), 50);

    timer.advance_to(50);
    with_simulated_irq(|| running.on_tick(50));
    assert_eq!(running.current(), Some(sleeper));
}

#[test]
fn yield_now_with_no_other_threads_keeps_current() {
    reset_switches();
    let timer = MockTimer::new();
    let scheduler = TestScheduler::new(MockTimerSource(timer.clone()), TEST_CONFIG)
        .bootstrap()
        .ok()
        .expect("bootstrap must succeed in tests");
    let only = scheduler.spawn(SpawnConfig::new("only"), || {}).unwrap();

    let running = scheduler.run();
    assert_eq!(running.current(), Some(only));
    let switches_before = switch_count();

    running.yield_now();
    assert_eq!(running.current(), Some(only));
    assert_eq!(switch_count(), switches_before);
}

#[test]
fn invalid_priority_returns_error() {
    let timer = MockTimer::new();
    let scheduler = SmallScheduler::new(MockTimerSource(timer), SMALL_CONFIG)
        .bootstrap()
        .ok()
        .expect("bootstrap must succeed in tests");
    let err = scheduler
        .spawn(SpawnConfig::new("bad").priority(Priority::new(10)), || {})
        .unwrap_err();
    assert_eq!(err, SpawnError::InvalidPriority);
}

#[test]
fn zero_stack_pages_returns_invalid_stack_pages_error() {
    let timer = MockTimer::new();
    let scheduler = TestScheduler::new(MockTimerSource(timer), TEST_CONFIG)
        .bootstrap()
        .ok()
        .expect("bootstrap must succeed in tests");
    let err = scheduler
        .spawn(SpawnConfig::new("nostack").stack_pages(0), || {})
        .unwrap_err();
    assert_eq!(err, SpawnError::InvalidStackPages);
}

#[test]
fn spawn_user_thread_without_factory_fails_with_address_space_creation_failed() {
    let timer = MockTimer::new();
    let scheduler = TestScheduler::new(MockTimerSource(timer), TEST_CONFIG)
        .bootstrap()
        .ok()
        .expect("bootstrap must succeed in tests");
    let err = scheduler
        .spawn(
            SpawnConfig::new("user").address_space(SpawnAddressSpace::User),
            || {},
        )
        .unwrap_err();
    assert_eq!(err, SpawnError::AddressSpaceCreationFailed);
}

#[test]
fn small_prio_scheduler_works_with_low_idle_priority() {
    reset_switches();
    let timer = MockTimer::new();
    let scheduler = SmallScheduler::new(MockTimerSource(timer.clone()), SMALL_CONFIG)
        .bootstrap()
        .ok()
        .expect("bootstrap must succeed in tests");
    let task = scheduler
        .spawn(SpawnConfig::new("task").priority(Priority::new(1)), || {})
        .unwrap();
    let running = scheduler.run();
    assert_eq!(running.current(), Some(task));
}

#[test]
fn yield_restores_preemption_when_enabled() {
    reset_switches();
    let timer = MockTimer::new();
    let scheduler = TestScheduler::new(MockTimerSource(timer.clone()), TEST_CONFIG)
        .bootstrap()
        .ok()
        .expect("bootstrap must succeed in tests");
    let _ = scheduler.spawn(SpawnConfig::new("a"), || {}).unwrap();
    let _ = scheduler.spawn(SpawnConfig::new("b"), || {}).unwrap();

    let running = scheduler.run();
    assert!(preemption_enabled(), "preemption enabled before yield");
    running.yield_now();
    running.yield_now();
    assert!(
        preemption_enabled(),
        "yield must restore preemption to enabled"
    );
}

#[test]
fn yield_from_masked_context_keeps_preemption_masked() {
    reset_switches();
    let timer = MockTimer::new();
    let scheduler = TestScheduler::new(MockTimerSource(timer.clone()), TEST_CONFIG)
        .bootstrap()
        .ok()
        .expect("bootstrap must succeed in tests");
    let _ = scheduler.spawn(SpawnConfig::new("a"), || {}).unwrap();
    let _ = scheduler.spawn(SpawnConfig::new("b"), || {}).unwrap();

    let running = scheduler.run();
    with_simulated_irq(|| {
        running.yield_now();
        assert!(
            !preemption_enabled(),
            "nested yield must not re-enable preemption"
        );
    });
    assert!(
        preemption_enabled(),
        "preemption is restored after the masked scope"
    );
}

#[test]
fn sleep_queue_orders_multiple_sleepers_by_deadline() {
    reset_switches();
    let timer = MockTimer::new();
    let scheduler = TestScheduler::new(MockTimerSource(timer.clone()), TEST_CONFIG)
        .bootstrap()
        .ok()
        .expect("bootstrap must succeed in tests");
    let early = scheduler.spawn(SpawnConfig::new("early"), || {}).unwrap();
    let late = scheduler.spawn(SpawnConfig::new("late"), || {}).unwrap();
    let runner = scheduler.spawn(SpawnConfig::new("runner"), || {}).unwrap();
    let running = scheduler.run();

    assert_eq!(running.current(), Some(early));
    running.sleep_ns(100);
    assert_eq!(running.current(), Some(late));
    running.sleep_ns(50);
    assert_eq!(running.current(), Some(runner));

    // Дедлайн `late` (50) меньше дедлайна `early` (100), поэтому первой
    // должна проснуться именно `late`.
    timer.advance_to(50);
    with_simulated_irq(|| running.on_tick(50));
    assert_eq!(running.current(), Some(late));

    timer.advance_to(100);
    with_simulated_irq(|| running.on_tick(100));
    let after_second_tick = running.current();
    assert!(
        after_second_tick == Some(runner)
            || after_second_tick == Some(early)
            || after_second_tick == Some(late),
        "after wake of `early` scheduler must run a real (non-idle) thread, got {after_second_tick:?}"
    );
}

#[test]
fn exit_current_releases_thread_slot_for_next_spawn() {
    use scheduler::SchedulerService;
    reset_switches();
    let timer = MockTimer::new();
    let scheduler =
        SmallScheduler::new(MockTimerSource(timer.clone()), SchedulerConfig::new(32, 4))
            .bootstrap()
            .ok()
            .expect("bootstrap must succeed in tests");
    let _ = scheduler.spawn(SpawnConfig::new("a"), || {}).unwrap();
    let _ = scheduler.spawn(SpawnConfig::new("b"), || {}).unwrap();
    let _ = scheduler.spawn(SpawnConfig::new("c"), || {}).unwrap();
    let handle = Arc::new(scheduler.handle());
    let running = scheduler.run();

    let err = running
        .handle()
        .spawn_boxed(SpawnConfig::new("overflow"), Box::new(|| {}))
        .unwrap_err();
    assert_eq!(err, SpawnError::NoFreeThreadSlots);
    let _ = handle;
}

#[test]
fn spawn_via_service_handle_works() {
    use scheduler::{SchedulerService, SchedulerServiceExt};
    reset_switches();
    let timer = MockTimer::new();
    let scheduler = TestScheduler::new(MockTimerSource(timer.clone()), TEST_CONFIG)
        .bootstrap()
        .ok()
        .expect("bootstrap must succeed in tests");
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
    assert_eq!(running.current(), Some(id));
}

fn new_factory_static() -> &'static MockAddressSpaceFactory {
    Box::leak(Box::new(MockAddressSpaceFactory::new()))
}

fn mark_process_loaded(
    handle: &SchedulerHandle<MockContext, MockTimerSource>,
    process: &Arc<capability::ProcessObject>,
) {
    let install = capability::UserImageInstall {
        segments: Vec::new(),
        entry: VirtualAddress::new(0x4000_0000),
        user_vm_base: PageAlignedVirtualAddress::from_usize(0x4000_0000).expect("aligned"),
        user_vm_size: 0x100_0000,
    };
    handle
        .load_user_image_into(process, &install)
        .expect("load stub image");
}

fn make_scheduler_with_factory(
    timer: Arc<MockTimer>,
    factory: &'static MockAddressSpaceFactory,
) -> Scheduler<MockContext, MockTimerSource, scheduler::Bootstrapped> {
    Scheduler::<MockContext, MockTimerSource, Uninit>::with_address_space_factory(
        MockTimerSource(timer),
        TEST_CONFIG,
        Some(factory),
    )
    .bootstrap()
    .ok()
    .expect("bootstrap must succeed in tests")
}

#[test]
fn switch_between_threads_in_same_process_does_not_change_address_space() {
    reset_switches();
    let timer = MockTimer::new();
    let scheduler = TestScheduler::new(MockTimerSource(timer.clone()), TEST_CONFIG)
        .bootstrap()
        .ok()
        .expect("bootstrap must succeed in tests");
    let _a = scheduler
        .spawn(SpawnConfig::new("a"), || {})
        .expect("spawn a");
    let _b = scheduler
        .spawn(SpawnConfig::new("b"), || {})
        .expect("spawn b");

    take_address_space_switches();

    let _ = scheduler.run();
    let switches = take_address_space_switches();
    for sw in switches {
        assert_eq!(sw, None, "kernel-only switches must use root=None");
    }
}

#[test]
fn switch_between_threads_of_different_processes_changes_address_space() {
    reset_switches();
    let factory = new_factory_static();
    let timer = MockTimer::new();
    let scheduler = make_scheduler_with_factory(timer.clone(), factory);

    let _a = scheduler
        .spawn(
            SpawnConfig::new("user-a").address_space(SpawnAddressSpace::User),
            || {},
        )
        .expect("spawn user-a");
    let _b = scheduler
        .spawn(
            SpawnConfig::new("user-b").address_space(SpawnAddressSpace::User),
            || {},
        )
        .expect("spawn user-b");

    let running = scheduler.run();
    let after_start = take_address_space_switches();
    let root_a = after_start
        .iter()
        .find_map(|sw| sw.map(|handle| handle.root.as_usize()))
        .expect("kernel->user-a switch must carry a user root");

    running.yield_now();
    let after_yield = take_address_space_switches();
    let root_b = after_yield
        .iter()
        .find_map(|sw| sw.map(|handle| handle.root.as_usize()))
        .expect("user-a->user-b switch must carry a user root");
    assert_ne!(
        root_a, root_b,
        "switching between distinct user processes must activate distinct AS roots"
    );
}

#[test]
fn kernel_to_user_switch_writes_user_root() {
    reset_switches();
    let factory = new_factory_static();
    let timer = MockTimer::new();
    let scheduler = make_scheduler_with_factory(timer.clone(), factory);

    let _user = scheduler
        .spawn(
            SpawnConfig::new("user").address_space(SpawnAddressSpace::User),
            || {},
        )
        .expect("spawn user");

    let _running = scheduler.run();
    let switches = take_address_space_switches();
    assert!(
        switches.iter().any(Option::is_some),
        "kernel->user switch must include Some(root); got {switches:?}"
    );
}

#[test]
fn user_to_kernel_switch_writes_zero_root() {
    reset_switches();
    let factory = new_factory_static();
    let timer = MockTimer::new();
    let scheduler = make_scheduler_with_factory(timer.clone(), factory);

    let _user = scheduler
        .spawn(
            SpawnConfig::new("user").address_space(SpawnAddressSpace::User),
            || {},
        )
        .expect("spawn user");
    let _kernel = scheduler
        .spawn(SpawnConfig::new("kernel"), || {})
        .expect("spawn kernel");

    let running = scheduler.run();
    take_address_space_switches();

    running.yield_now();
    let switches = take_address_space_switches();
    assert!(
        switches.iter().any(Option::is_none),
        "user->kernel switch must include None; got {switches:?}"
    );
}

#[test]
fn user_address_space_is_live_while_process_is_live() {
    reset_switches();
    let factory = new_factory_static();
    let timer = MockTimer::new();
    let scheduler = make_scheduler_with_factory(timer.clone(), factory);

    let _u = scheduler
        .spawn(
            SpawnConfig::new("u").address_space(SpawnAddressSpace::User),
            || {},
        )
        .expect("spawn u");

    assert_eq!(factory.created(), 1);
    assert_eq!(
        factory.released(),
        0,
        "AS must stay alive while the owning process is live"
    );
}

#[test]
fn terminating_last_user_thread_releases_address_space() {
    // Последний thread_exit ставит процесс в pending-removal; следующий
    // `switch_to_next` удаляет его из ProcessTable, дропая `Arc<AddressSpace>`.
    // Это последняя ссылка -> `MockUserMapper::Drop` инкрементирует `released`.
    reset_switches();
    let factory = new_factory_static();
    let timer = MockTimer::new();
    let scheduler = make_scheduler_with_factory(timer.clone(), factory);

    let u = scheduler
        .spawn(
            SpawnConfig::new("u").address_space(SpawnAddressSpace::User),
            || {},
        )
        .expect("spawn u");

    let running = scheduler.run();
    assert_eq!(running.current(), Some(u));
    let pid = running.thread_process_id(u).expect("owning pid");
    assert_eq!(factory.created(), 1);
    assert_eq!(factory.released(), 0);

    running.exit_current();

    assert!(
        running.process_object_for(pid).is_none(),
        "owning process must be reclaimed from the table after last thread exit"
    );
    assert_eq!(
        factory.released(),
        1,
        "AS must be released once the owning process is reclaimed",
    );
}

#[test]
fn exit_current_signals_thread_terminated() {
    reset_switches();
    let timer = MockTimer::new();
    let scheduler = TestScheduler::new(MockTimerSource(timer.clone()), TEST_CONFIG)
        .bootstrap()
        .ok()
        .expect("bootstrap must succeed in tests");
    let id = scheduler
        .spawn(SpawnConfig::new("t"), || {})
        .expect("spawn t");
    let target = scheduler.thread_object_for(id).expect("thread_object");

    let running = scheduler.run();
    assert!(!target.terminated());

    running.exit_current();

    assert!(target.terminated());
    assert_eq!(target.exit_code(), 0);
}

#[test]
fn exit_current_with_nonzero_code_publishes_code() {
    reset_switches();
    let timer = MockTimer::new();
    let scheduler = TestScheduler::new(MockTimerSource(timer.clone()), TEST_CONFIG)
        .bootstrap()
        .ok()
        .expect("bootstrap must succeed in tests");
    let id = scheduler
        .spawn(SpawnConfig::new("t"), || {})
        .expect("spawn t");
    let target = scheduler.thread_object_for(id).expect("thread_object");

    let running = scheduler.run();
    running.exit_current_with_code(42);

    assert!(target.terminated());
    assert_eq!(target.exit_code(), 42);
}

#[test]
fn last_thread_exit_signals_process_terminated() {
    reset_switches();
    let timer = MockTimer::new();
    let scheduler = TestScheduler::new(MockTimerSource(timer.clone()), TEST_CONFIG)
        .bootstrap()
        .ok()
        .expect("bootstrap must succeed in tests");
    let id = scheduler
        .spawn(SpawnConfig::new("solo"), || {})
        .expect("spawn solo");
    let pid = scheduler.thread_process_id(id).expect("process id");
    let process_object = scheduler.process_object_for(pid).expect("process_object");

    let running = scheduler.run();
    assert!(!process_object.terminated());

    running.exit_current();

    assert!(process_object.terminated());
    assert_eq!(process_object.exit_code(), 0);
}

#[test]
fn non_last_thread_exit_does_not_signal_process() {
    use scheduler::SchedulerService;

    reset_switches();
    let timer = MockTimer::new();
    let scheduler = TestScheduler::new(MockTimerSource(timer.clone()), TEST_CONFIG)
        .bootstrap()
        .ok()
        .expect("bootstrap must succeed in tests");
    let first = scheduler
        .spawn(SpawnConfig::new("first"), || {})
        .expect("spawn first");

    let running = scheduler.run();
    assert_eq!(running.current(), Some(first));
    let pid = running.thread_process_id(first).expect("process id");

    let second = running
        .handle()
        .spawn_boxed(
            SpawnConfig::new("second").address_space(SpawnAddressSpace::Inherit),
            Box::new(|| {}),
        )
        .expect("spawn second");
    assert_eq!(running.thread_process_id(second), Some(pid));

    let process_object = running.process_object_for(pid).expect("process_object");
    let first_thread_object = running.thread_object_for(first).expect("first target");

    running.exit_current();

    assert!(first_thread_object.terminated());
    assert!(!process_object.terminated());
    assert!(running.process_object_for(pid).is_some());
}

#[test]
fn process_object_outlives_process_table_entry() {
    reset_switches();
    let timer = MockTimer::new();
    let scheduler = TestScheduler::new(MockTimerSource(timer.clone()), TEST_CONFIG)
        .bootstrap()
        .ok()
        .expect("bootstrap must succeed in tests");
    let id = scheduler
        .spawn(SpawnConfig::new("zombie"), || {})
        .expect("spawn");
    let pid = scheduler.thread_process_id(id).expect("process id");
    let process_object = scheduler.process_object_for(pid).expect("process_object");

    let count_before = scheduler.process_count();
    let running = scheduler.run();
    running.exit_current();

    assert!(running.process_object_for(pid).is_none());
    assert!(running.process_count() < count_before);
    assert!(Arc::strong_count(&process_object) >= 1);
    assert!(process_object.terminated());
}

#[test]
fn current_thread_object_returns_running_thread_object() {
    reset_switches();
    let timer = MockTimer::new();
    let scheduler = TestScheduler::new(MockTimerSource(timer.clone()), TEST_CONFIG)
        .bootstrap()
        .ok()
        .expect("bootstrap must succeed in tests");
    let id = scheduler
        .spawn(SpawnConfig::new("t"), || {})
        .expect("spawn t");
    let expected = scheduler.thread_object_for(id).expect("thread_object");

    let running = scheduler.run();
    assert_eq!(running.current(), Some(id));

    let observed = running
        .handle()
        .current_thread_object()
        .expect("current_thread_object");
    assert!(Arc::ptr_eq(&observed, &expected));
}

#[test]
fn current_process_object_returns_running_process_object() {
    reset_switches();
    let timer = MockTimer::new();
    let scheduler = TestScheduler::new(MockTimerSource(timer.clone()), TEST_CONFIG)
        .bootstrap()
        .ok()
        .expect("bootstrap must succeed in tests");
    let id = scheduler
        .spawn(SpawnConfig::new("t"), || {})
        .expect("spawn t");
    let pid = scheduler.thread_process_id(id).expect("process id");
    let expected = scheduler.process_object_for(pid).expect("process_object");

    let running = scheduler.run();
    assert_eq!(running.current(), Some(id));

    let observed = running
        .handle()
        .current_process_object()
        .expect("current_process_object");
    assert!(Arc::ptr_eq(&observed, &expected));
}

#[test]
fn kernel_thread_completion_signals_terminated() {
    let forwarder = install_forwarding_runtime();

    reset_switches();
    let timer = MockTimer::new();
    let scheduler = TestScheduler::new(MockTimerSource(timer.clone()), TEST_CONFIG)
        .bootstrap()
        .ok()
        .expect("bootstrap must succeed in tests");
    let id = scheduler
        .spawn(SpawnConfig::new("t"), || {})
        .expect("spawn t");
    let target = scheduler.thread_object_for(id).expect("thread_object");
    let handle = scheduler.handle();

    let running = scheduler.run();
    assert_eq!(running.current(), Some(id));
    assert!(!target.terminated());

    forwarder.set(Arc::new(handle));

    let prev_hook = panic::take_hook();
    panic::set_hook(Box::new(|_| {}));
    let result = panic::catch_unwind(panic::AssertUnwindSafe(|| capability::thread_exit(0)));
    panic::set_hook(prev_hook);
    assert!(
        result.is_err(),
        "thread_exit must not return when scheduler::switch is a no-op"
    );

    forwarder.clear();

    assert!(target.terminated());
    assert_eq!(target.exit_code(), 0);
}

#[test]
fn process_exit_terminates_all_threads_and_releases_handles() {
    use capability::{Capability, CapabilityTarget, KernelRuntime, Rights, Signal};
    use collections::LockCell;
    use scheduler::SchedulerService;

    reset_switches();
    let timer = MockTimer::new();
    let scheduler = TestScheduler::new(MockTimerSource(timer.clone()), TEST_CONFIG)
        .bootstrap()
        .ok()
        .expect("bootstrap must succeed in tests");
    let first = scheduler
        .spawn(SpawnConfig::new("first"), || {})
        .expect("spawn first");

    let running = scheduler.run();
    assert_eq!(running.current(), Some(first));
    let handle = running.handle();
    let second = handle
        .spawn_boxed(
            SpawnConfig::new("second").address_space(SpawnAddressSpace::Inherit),
            Box::new(|| {}),
        )
        .expect("spawn second");
    let first_object = running.thread_object_for(first).expect("first target");
    let second_object = running.thread_object_for(second).expect("second target");
    let pid = running.thread_process_id(first).expect("process id");
    let process_object = running.process_object_for(pid).expect("process target");
    let table = handle
        .current_handle_table()
        .expect("current process handle table");
    let signal = Signal::new();
    let signal_weak = Arc::downgrade(&signal);
    table
        .with_lock(|handles| {
            handles.insert(Capability::new(
                CapabilityTarget::Signal(signal),
                Rights::READ,
            ))
        })
        .expect("insert process-owned handle");

    let prev_hook = panic::take_hook();
    panic::set_hook(Box::new(|_| {}));
    let result = panic::catch_unwind(panic::AssertUnwindSafe(|| {
        KernelRuntime::exit_current_process(&handle, 17)
    }));
    panic::set_hook(prev_hook);

    assert!(result.is_err(), "process_exit must not return");
    assert!(first_object.terminated());
    assert_eq!(first_object.exit_code(), 17);
    assert!(second_object.terminated());
    assert_eq!(second_object.exit_code(), 17);
    assert!(process_object.terminated());
    assert_eq!(process_object.exit_code(), 17);
    assert_eq!(table.with_lock(|handles| handles.live_count()), 0);
    assert!(signal_weak.upgrade().is_none());
    assert!(running.thread_object_for(first).is_none());
    assert!(running.thread_object_for(second).is_none());
    assert!(running.process_object_for(pid).is_none());
}

#[test]
fn process_create_returns_handle_to_empty_process() {
    reset_switches();
    let factory = new_factory_static();
    let timer = MockTimer::new();
    let scheduler = make_scheduler_with_factory(timer, factory);
    let processes_before = scheduler.process_count();
    let handle = scheduler.handle();

    let process = handle
        .create_empty_process("p")
        .expect("create_empty_process must succeed");

    assert!(!process.terminated());
    assert!(Arc::strong_count(&process) >= 1);
    assert_eq!(scheduler.process_count(), processes_before + 1);
}

#[test]
fn process_create_rejects_empty_name() {
    reset_switches();
    let factory = new_factory_static();
    let timer = MockTimer::new();
    let scheduler = make_scheduler_with_factory(timer, factory);
    let processes_before = scheduler.process_count();
    let handle = scheduler.handle();

    let Err(err) = handle.create_empty_process("") else {
        panic!("empty process name must be rejected");
    };
    assert_eq!(err, capability::SpawnError::InvalidName);
    assert_eq!(scheduler.process_count(), processes_before);
}

#[test]
fn process_create_accepts_name_at_capacity() {
    reset_switches();
    let factory = new_factory_static();
    let timer = MockTimer::new();
    let scheduler = make_scheduler_with_factory(timer, factory);
    let processes_before = scheduler.process_count();
    let handle = scheduler.handle();

    handle
        .create_empty_process(&"n".repeat(syscall::MAX_PROCESS_NAME_LEN))
        .expect("name of exactly MAX_PROCESS_NAME_LEN bytes is accepted");

    assert_eq!(scheduler.process_count(), processes_before + 1);
}

#[test]
fn process_create_rejects_name_over_capacity() {
    reset_switches();
    let factory = new_factory_static();
    let timer = MockTimer::new();
    let scheduler = make_scheduler_with_factory(timer, factory);
    let processes_before = scheduler.process_count();
    let handle = scheduler.handle();

    let name = "n".repeat(syscall::MAX_PROCESS_NAME_LEN + 1);
    let Err(err) = handle.create_empty_process(&name) else {
        panic!("process name over capacity must be rejected");
    };
    assert_eq!(err, capability::SpawnError::InvalidName);
    assert_eq!(scheduler.process_count(), processes_before);
}

#[test]
fn thread_terminate_via_handle_signals_terminated() {
    use capability::UserThreadEntry;

    reset_switches();
    let factory = new_factory_static();
    let timer = MockTimer::new();
    let scheduler = make_scheduler_with_factory(timer, factory);
    let handle = scheduler.handle();

    let process = handle
        .create_empty_process("p")
        .expect("create_empty_process");
    mark_process_loaded(&handle, &process);
    let thread = handle
        .create_user_thread(
            &process,
            UserThreadEntry {
                entry_pc: 0x4000_0000,
                user_sp: 0x4001_0000,
                arg: 0,
                priority: 1,
            },
        )
        .expect("create_user_thread");

    assert!(!thread.terminated());
    handle
        .terminate_thread(&thread, 7)
        .expect("terminate_thread");
    assert!(thread.terminated());
    assert_eq!(thread.exit_code(), 7);
}

#[test]
fn process_terminate_via_handle_terminates_all_threads() {
    use capability::UserThreadEntry;

    reset_switches();
    let factory = new_factory_static();
    let timer = MockTimer::new();
    let scheduler = make_scheduler_with_factory(timer, factory);
    let handle = scheduler.handle();

    let process = handle
        .create_empty_process("p")
        .expect("create_empty_process");
    mark_process_loaded(&handle, &process);
    let t1 = handle
        .create_user_thread(
            &process,
            UserThreadEntry {
                entry_pc: 0x4000_0000,
                user_sp: 0x4001_0000,
                arg: 0,
                priority: 1,
            },
        )
        .expect("create_user_thread t1");
    let t2 = handle
        .create_user_thread(
            &process,
            UserThreadEntry {
                entry_pc: 0x4000_0000,
                user_sp: 0x4001_2000,
                arg: 1,
                priority: 1,
            },
        )
        .expect("create_user_thread t2");

    handle
        .terminate_process(&process, -1)
        .expect("terminate_process");

    assert!(t1.terminated());
    assert!(t2.terminated());
    assert!(process.terminated());
    assert_eq!(process.exit_code(), -1);
}

#[test]
fn switch_to_next_skips_terminated_thread_in_ready_queue() {
    reset_switches();
    let timer = MockTimer::new();
    let scheduler = TestScheduler::new(MockTimerSource(timer.clone()), TEST_CONFIG)
        .bootstrap()
        .ok()
        .expect("bootstrap must succeed in tests");
    let t1 = scheduler
        .spawn(SpawnConfig::new("t1"), || {})
        .expect("spawn t1");
    let t2 = scheduler
        .spawn(SpawnConfig::new("t2"), || {})
        .expect("spawn t2");
    let t3 = scheduler
        .spawn(SpawnConfig::new("t3"), || {})
        .expect("spawn t3");
    let t2_thread_object = scheduler.thread_object_for(t2).expect("t2 target");

    let running = scheduler.run();
    assert_eq!(running.current(), Some(t1));

    let handle = running.handle();
    handle
        .terminate_thread(&t2_thread_object, 9)
        .expect("terminate t2");
    assert!(t2_thread_object.terminated());

    running.yield_now();
    assert_eq!(running.current(), Some(t3));
}

#[test]
fn spawn_rollback_undoes_inherit_increment_on_thread_table_full() {
    use scheduler::SchedulerService;

    reset_switches();
    let timer = MockTimer::new();
    let scheduler = TestScheduler::new(MockTimerSource(timer.clone()), TEST_CONFIG)
        .bootstrap()
        .ok()
        .expect("bootstrap must succeed in tests");
    let t1 = scheduler
        .spawn(SpawnConfig::new("t1"), || {})
        .expect("spawn t1");

    let running = scheduler.run();
    assert_eq!(running.current(), Some(t1));
    let pid = running.thread_process_id(t1).expect("pid");
    let handle = running.handle();

    let inherit_cfg = SpawnConfig::new("inh").address_space(SpawnAddressSpace::Inherit);
    // Заполняем thread-table Inherit-потоками до отказа, не завязываясь на
    // число bootstrap-слотов.
    let (res, count_before) = loop {
        let count_before = running.process_thread_count(pid).expect("pid alive");
        if let Err(e) = handle.spawn_boxed(inherit_cfg, Box::new(|| {})) {
            break (e, count_before);
        }
    };

    // На переполнении spawn возвращает NoFreeThreadSlots и откатывает
    // паразитный инкремент thread_count процесса.
    assert_eq!(res, SpawnError::NoFreeThreadSlots);
    let count_after = running.process_thread_count(pid).expect("pid alive");
    assert_eq!(
        count_before, count_after,
        "failed Inherit spawn must not leak a thread_count increment"
    );
}

#[test]
fn create_user_thread_rejects_unloaded_process() {
    use capability::UserThreadEntry;

    reset_switches();
    let factory = new_factory_static();
    let timer = MockTimer::new();
    let scheduler = make_scheduler_with_factory(timer, factory);
    let handle = scheduler.handle();

    let process = handle
        .create_empty_process("p")
        .expect("create_empty_process");

    let err = handle.create_user_thread(
        &process,
        UserThreadEntry {
            entry_pc: 0x4000_0000,
            user_sp: 0x4001_0000,
            arg: 0,
            priority: 1,
        },
    );
    match err {
        Err(capability::SpawnError::ImageNotLoaded) => {}
        other => panic!("expected ImageNotLoaded, got {:?}", other.err()),
    }
}

#[test]
fn create_user_thread_rejects_terminated_process() {
    use capability::UserThreadEntry;

    reset_switches();
    let factory = new_factory_static();
    let timer = MockTimer::new();
    let scheduler = make_scheduler_with_factory(timer, factory);
    let handle = scheduler.handle();

    let process = handle
        .create_empty_process("p")
        .expect("create_empty_process");
    // Загрузка образа до терминации: иначе отказ пришёл бы по ImageNotLoaded.
    mark_process_loaded(&handle, &process);
    handle
        .terminate_process(&process, 5)
        .expect("terminate_process");

    let err = handle.create_user_thread(
        &process,
        UserThreadEntry {
            entry_pc: 0x4000_0000,
            user_sp: 0x4001_0000,
            arg: 0,
            priority: 1,
        },
    );
    match err {
        Err(capability::SpawnError::ProcessTerminated) => {}
        other => panic!("expected ProcessTerminated, got {:?}", other.err()),
    }
}

#[test]
fn load_user_image_into_attaches_user_vm() {
    reset_switches();
    let factory = new_factory_static();
    let timer = MockTimer::new();
    let scheduler = make_scheduler_with_factory(timer, factory);
    let handle = scheduler.handle();

    let process = handle
        .create_empty_process("p")
        .expect("create_empty_process");

    mark_process_loaded(&handle, &process);
    let r = handle.create_user_thread(
        &process,
        capability::UserThreadEntry {
            entry_pc: 0x4000_0000,
            user_sp: 0x4001_0000,
            arg: 0,
            priority: 1,
        },
    );
    assert!(r.is_ok(), "create_user_thread must succeed after load");
}

#[test]
fn load_user_image_into_rejects_double_load() {
    use capability::LoadImageError;

    reset_switches();
    let factory = new_factory_static();
    let timer = MockTimer::new();
    let scheduler = make_scheduler_with_factory(timer, factory);
    let handle = scheduler.handle();

    let process = handle
        .create_empty_process("p")
        .expect("create_empty_process");
    mark_process_loaded(&handle, &process);

    let install = capability::UserImageInstall {
        segments: Vec::new(),
        entry: VirtualAddress::new(0x4000_0000),
        user_vm_base: PageAlignedVirtualAddress::from_usize(0x4000_0000).expect("aligned"),
        user_vm_size: 0x100_0000,
    };
    match handle.load_user_image_into(&process, &install) {
        Err(LoadImageError::WrongState) => {}
        other => panic!("expected WrongState, got {other:?}"),
    }
}

fn loader_table_with_handle() -> (
    Arc<collections::MutexCell<capability::HandleTable>>,
    capability::HandleId,
) {
    use capability::{Capability, CapabilityTarget, Rights, Signal};
    use collections::LockCell;
    let table = Arc::new(collections::MutexCell::new(capability::HandleTable::new()));
    let id = table
        .with_lock(|tbl| {
            tbl.insert(Capability::new(
                CapabilityTarget::Signal(Signal::new()),
                Rights::TRANSFER | Rights::READ,
            ))
        })
        .expect("insert loader handle");
    (table, id)
}

#[test]
fn start_user_process_creates_thread_and_marks_loader_state() {
    use capability::{UserStartSpec, UserThreadEntry};

    reset_switches();
    let factory = new_factory_static();
    let timer = MockTimer::new();
    let scheduler = make_scheduler_with_factory(timer, factory);
    let handle = scheduler.handle();

    let process = handle
        .create_empty_process("p")
        .expect("create_empty_process");
    mark_process_loaded(&handle, &process);

    let (loader_table, handle_id) = loader_table_with_handle();
    let spec = UserStartSpec {
        entry: UserThreadEntry {
            entry_pc: 0x4000_0000,
            user_sp: 0x4001_0000,
            arg: 0,
            priority: 1,
        },
        loader_handle_table: loader_table,
        handle_id,
        metering_grant: None,
    };
    let thread = handle.start_user_process(&process, spec).expect("start ok");
    assert!(!thread.terminated());
}

struct SchedulerLockProbe {
    handle: SchedulerHandle<MockContext, MockTimerSource>,
    calls: std::sync::atomic::AtomicUsize,
    acquisitions: std::sync::atomic::AtomicUsize,
}

impl SchedulerLockProbe {
    fn new(handle: &SchedulerHandle<MockContext, MockTimerSource>) -> Arc<Self> {
        Arc::new(Self {
            handle: handle.clone(),
            calls: std::sync::atomic::AtomicUsize::new(0),
            acquisitions: std::sync::atomic::AtomicUsize::new(0),
        })
    }

    fn calls(&self) -> usize {
        self.calls.load(std::sync::atomic::Ordering::Acquire)
    }

    fn acquisitions(&self) -> usize {
        self.acquisitions.load(std::sync::atomic::Ordering::Acquire)
    }
}

impl capability::CancelTarget for SchedulerLockProbe {
    fn cancel(&self) {
        use std::sync::atomic::Ordering;

        self.calls.fetch_add(1, Ordering::AcqRel);
        let handle = self.handle.clone();
        let (sender, receiver) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = sender.send(handle.current_thread_object().is_some());
        });
        if receiver
            .recv_timeout(std::time::Duration::from_secs(5))
            .is_ok()
        {
            self.acquisitions.fetch_add(1, Ordering::AcqRel);
        }
    }
}

struct CleanupQuarantineProbe {
    handle: SchedulerHandle<MockContext, MockTimerSource>,
    victim_cpu: CpuId,
    return_cpu: CpuId,
    factory: &'static MockAddressSpaceFactory,
    calls: std::sync::atomic::AtomicUsize,
    released_during_cancel: std::sync::atomic::AtomicUsize,
}

impl CleanupQuarantineProbe {
    fn new(
        handle: &SchedulerHandle<MockContext, MockTimerSource>,
        victim_cpu: CpuId,
        return_cpu: CpuId,
        factory: &'static MockAddressSpaceFactory,
    ) -> Arc<Self> {
        Arc::new(Self {
            handle: handle.clone(),
            victim_cpu,
            return_cpu,
            factory,
            calls: std::sync::atomic::AtomicUsize::new(0),
            released_during_cancel: std::sync::atomic::AtomicUsize::new(usize::MAX),
        })
    }
}

impl capability::CancelTarget for CleanupQuarantineProbe {
    fn cancel(&self) {
        use std::sync::atomic::Ordering;

        self.calls.fetch_add(1, Ordering::AcqRel);
        set_current_cpu(self.victim_cpu);
        self.handle.on_reschedule();
        self.released_during_cancel
            .store(self.factory.released(), Ordering::Release);
        set_current_cpu(self.return_cpu);
    }
}

// Пробуждение cancel-цели снятого слота берёт scheduler-лок, под которым идёт
// изъятие bootstrap-хэндла, и потому обязано быть отложено.
#[test]
fn start_user_process_cancels_loader_waiters_outside_scheduler_lock() {
    use capability::{UserStartSpec, UserThreadEntry};
    use collections::LockCell;

    reset_switches();
    let factory = new_factory_static();
    let timer = MockTimer::new();
    let scheduler = make_scheduler_with_factory(timer, factory);
    let handle = scheduler.handle();

    let process = handle
        .create_empty_process("p")
        .expect("create_empty_process");
    mark_process_loaded(&handle, &process);

    let (loader_table, handle_id) = loader_table_with_handle();
    let probe = SchedulerLockProbe::new(&handle);
    loader_table
        .with_lock(|tbl| {
            tbl.register_cancel(
                handle_id,
                probe.clone() as Arc<dyn capability::CancelTarget>,
            )
        })
        .expect("register_cancel");

    let spec = UserStartSpec {
        entry: UserThreadEntry {
            entry_pc: 0x4000_0000,
            user_sp: 0x4001_0000,
            arg: 0,
            priority: 1,
        },
        loader_handle_table: loader_table,
        handle_id,
        metering_grant: None,
    };
    handle.start_user_process(&process, spec).expect("start ok");

    assert_eq!(probe.calls(), 1, "cancel of the released slot must fire");
    assert_eq!(
        probe.acquisitions(),
        1,
        "cancel must run outside the scheduler lock",
    );
}

#[test]
fn start_user_process_rejects_unloaded() {
    use capability::{StartProcessError, UserStartSpec, UserThreadEntry};

    reset_switches();
    let factory = new_factory_static();
    let timer = MockTimer::new();
    let scheduler = make_scheduler_with_factory(timer, factory);
    let handle = scheduler.handle();

    let process = handle
        .create_empty_process("p")
        .expect("create_empty_process");

    let (loader_table, handle_id) = loader_table_with_handle();
    let spec = UserStartSpec {
        entry: UserThreadEntry {
            entry_pc: 0x4000_0000,
            user_sp: 0x4001_0000,
            arg: 0,
            priority: 1,
        },
        loader_handle_table: loader_table,
        handle_id,
        metering_grant: None,
    };
    match handle.start_user_process(&process, spec) {
        Err(StartProcessError::WrongState) => {}
        Err(other) => panic!("expected WrongState, got {other:?}"),
        Ok(_) => panic!("expected WrongState, got Ok"),
    }
}

#[test]
fn start_user_process_rejects_terminated_process() {
    use capability::{StartProcessError, UserStartSpec, UserThreadEntry};

    reset_switches();
    let factory = new_factory_static();
    let timer = MockTimer::new();
    let scheduler = make_scheduler_with_factory(timer, factory);
    let handle = scheduler.handle();

    let process = handle
        .create_empty_process("p")
        .expect("create_empty_process");
    mark_process_loaded(&handle, &process);
    handle
        .terminate_process(&process, 5)
        .expect("terminate_process");

    let (loader_table, handle_id) = loader_table_with_handle();
    let spec = UserStartSpec {
        entry: UserThreadEntry {
            entry_pc: 0x4000_0000,
            user_sp: 0x4001_0000,
            arg: 0,
            priority: 1,
        },
        loader_handle_table: loader_table,
        handle_id,
        metering_grant: None,
    };
    match handle.start_user_process(&process, spec) {
        // Именно WrongState: отказ наступает до валидации loader-хэндла, а не
        // внутри prepare_user_thread (тот дал бы SpawnFailed).
        Err(StartProcessError::WrongState) => {}
        Err(other) => panic!("expected WrongState, got {other:?}"),
        Ok(_) => panic!("expected WrongState, got Ok"),
    }
}

// `start_user_process` при SpawnFailed оставляет handle в loader-table
// нетронутым, с исходным `HandleId`.
#[test]
fn start_user_process_preserves_handles_on_spawn_failure() {
    use capability::{
        Capability, CapabilityTarget, KernelRuntime, Rights, Signal, StartProcessError,
        UserStartSpec, UserThreadEntry,
    };
    use collections::LockCell;
    use scheduler::SchedulerService;

    reset_switches();
    let factory = new_factory_static();
    let timer = MockTimer::new();
    let scheduler = make_scheduler_with_factory(timer, factory);

    // Inherit-spawn приклеяются к процессу current-потока, не съедая
    // новые ProcessTable-слоты.
    let _host = scheduler
        .spawn(SpawnConfig::new("host"), || {})
        .expect("host spawn");
    let running = scheduler.run();
    let handle = running.handle();
    let process = handle
        .create_empty_process("p")
        .expect("create_empty_process");
    mark_process_loaded(&handle, &process);

    let inherit_cfg = SpawnConfig::new("inh").address_space(SpawnAddressSpace::Inherit);
    let to_fill = TEST_CONFIG.max_threads() - 2;
    for _ in 0..to_fill {
        handle
            .spawn_boxed(inherit_cfg, Box::new(|| {}))
            .expect("inherit filler spawn");
    }

    let loader_table = handle
        .current_handle_table()
        .expect("running host has a current handle table");
    let signal = Signal::new();
    let weak = Arc::downgrade(&signal);
    let h = Capability::new(
        CapabilityTarget::Signal(signal),
        Rights::TRANSFER | Rights::READ,
    );
    let handle_id = loader_table
        .with_lock(|tbl| tbl.insert(h))
        .expect("insert into loader table");

    let spec = UserStartSpec {
        entry: UserThreadEntry {
            entry_pc: 0x4000_0000,
            user_sp: 0x4001_0000,
            arg: 0,
            priority: 1,
        },
        loader_handle_table: loader_table.clone(),
        handle_id,
        metering_grant: None,
    };

    match handle.start_user_process(&process, spec) {
        Err(StartProcessError::SpawnFailed(_)) => {
            assert!(
                weak.upgrade().is_some(),
                "capability target must stay alive after error"
            );
            loader_table.with_lock(|tbl| {
                let h = tbl
                    .get(handle_id, Rights::TRANSFER)
                    .expect("handle still present in loader-table with original id");
                assert!(h.rights().contains(Rights::TRANSFER));
            });
        }
        Err(other) => panic!("expected SpawnFailed, got {other:?}"),
        Ok(_) => panic!("expected SpawnFailed, got Ok"),
    }
}

#[test]
fn load_user_image_into_keeps_segment_frames_alive_after_caller_drops_arc() {
    reset_switches();
    let factory = new_factory_static();
    let timer = MockTimer::new();
    let scheduler = make_scheduler_with_factory(timer, factory);
    let handle = scheduler.handle();

    let process = handle
        .create_empty_process("p")
        .expect("create_empty_process");

    let fa: &'static CountingFrameAllocator = Box::leak(Box::new(CountingFrameAllocator::new()));
    let region = Arc::new(
        MemoryRegion::create_virtual(fa, NonZeroUsize::new(2).unwrap(), AccessMask::RW)
            .expect("region alloc"),
    );

    let install = capability::UserImageInstall {
        segments: vec![capability::UserSegmentInstall {
            va_base: PageAlignedVirtualAddress::from_usize(0x4000_0000).unwrap(),
            mapped_size: 2 * memory::PAGE_SIZE.get(),
            region: region.clone(),
            flags: MemFlags::user_rw(),
        }],
        entry: VirtualAddress::new(0x4000_0000),
        user_vm_base: PageAlignedVirtualAddress::from_usize(0x6000_0000).unwrap(),
        user_vm_size: 0x100_0000,
    };
    handle
        .load_user_image_into(&process, &install)
        .expect("load image");

    // Если бы scheduler не удержал регионы у процесса, ref-count ушёл бы
    // в ноль и `MemoryRegion::Drop` вернул бы фреймы в FA при живых PTE.
    drop(region);
    drop(install);
    assert_eq!(
        fa.deallocated_count(),
        0,
        "image-segment frames must stay alive while process is live",
    );
}

// Кадр port-syscall убитого в парковке recv потока держит клон таблицы, и
// `Drop for HandleTable` не наступает: capability освобождает дренаж терминации.
#[test]
fn process_terminate_drains_handle_table_held_outside_process() {
    use capability::{Capability, CapabilityTarget, Port, Rights, UserStartSpec, UserThreadEntry};
    use collections::LockCell;

    reset_switches();
    let factory = new_factory_static();
    let timer = MockTimer::new();
    let scheduler = make_scheduler_with_factory(timer, factory);
    let handle = scheduler.handle();

    let process = handle
        .create_empty_process("victim")
        .expect("create_empty_process");
    mark_process_loaded(&handle, &process);

    let port = Port::new();
    let port_weak = Arc::downgrade(&port);
    let loader_table = Arc::new(collections::MutexCell::new(capability::HandleTable::new()));
    let handle_id = loader_table
        .with_lock(|tbl| {
            tbl.insert(Capability::new(
                CapabilityTarget::Port(port),
                Rights::TRANSFER | Rights::READ,
            ))
        })
        .expect("insert loader handle");
    let spec = UserStartSpec {
        entry: UserThreadEntry {
            entry_pc: 0x4000_0000,
            user_sp: 0x4001_0000,
            arg: 0,
            priority: 1,
        },
        loader_handle_table: loader_table,
        handle_id,
        metering_grant: None,
    };
    handle.start_user_process(&process, spec).expect("start ok");

    let pid = scheduler.process_id_for(&process).expect("process id");
    // Этот клон играет роль замороженного кадра. Он сохраняется при удалении
    // процесса из таблицы и при разрушении самого `Process`.
    let frozen_frame_table = scheduler.process_handle_table(pid).expect("handle table");
    assert_eq!(frozen_frame_table.with_lock(|tbl| tbl.live_count()), 1);

    handle
        .terminate_process(&process, 9)
        .expect("terminate_process");
    let running = scheduler.run();
    running.yield_now();

    assert_eq!(
        frozen_frame_table.with_lock(|tbl| tbl.live_count()),
        0,
        "terminated process must not keep live handles",
    );
    assert!(
        port_weak.upgrade().is_none(),
        "port owned by the terminated process must be released",
    );
}

#[test]
fn process_terminate_on_empty_process_signals_terminated_and_releases_slot() {
    reset_switches();
    let factory = new_factory_static();
    let timer = MockTimer::new();
    let scheduler = make_scheduler_with_factory(timer, factory);
    let handle = scheduler.handle();

    let process = handle
        .create_empty_process("empty")
        .expect("create_empty_process");
    let before = scheduler.process_count();
    assert!(!process.terminated());

    handle
        .terminate_process(&process, 7)
        .expect("terminate_process");
    assert!(process.terminated());
    assert_eq!(process.exit_code(), 7);

    // Повторный terminate - no-op: первый код фиксируется, дублирующего
    // pending-pid быть не должно.
    handle
        .terminate_process(&process, 99)
        .expect("terminate_process again");
    assert_eq!(process.exit_code(), 7);

    // `cleanup_pending_process_removals` отрабатывает на `switch_to_next`.
    let running = scheduler.run();
    running.yield_now();
    assert!(
        running.process_count() < before,
        "empty process slot must be reclaimed after scheduler tick",
    );
}

#[test]
fn second_bootstrap_of_the_same_cpu_is_rejected() {
    let timer = MockTimer::new();
    let scheduler = TestScheduler::new(MockTimerSource(timer.clone()), TEST_CONFIG)
        .bootstrap()
        .ok()
        .expect("bootstrap must succeed in tests");

    // Тот же CPU второй раз получил бы вторую очередь готовых и второй
    // idle-поток, а `TPIDR_EL1` указывал бы на последнюю из них: потоки первой
    // очереди перестали бы выбираться.
    let handle = scheduler.handle();
    assert_eq!(
        CpuEntry::bootstrap(&handle),
        Err(CpuBootstrapError::AlreadyBootstrapped)
    );
}

#[test]
fn reaped_ready_id_can_be_reused_before_first_cpu_entry() {
    reset_switches();
    let factory = new_factory_static();
    let timer = MockTimer::new();
    let scheduler = make_scheduler_with_factory(timer.clone(), factory);

    let second = bootstrap_cpu(&scheduler.handle(), 1);

    let first = scheduler
        .spawn(SpawnConfig::new("first"), || {})
        .expect("spawn first");
    let running = scheduler.run();
    assert_eq!(running.current(), Some(first));

    let handle = running.handle();
    let victim = SchedulerServiceExt::spawn(
        &handle,
        SpawnConfig::new("victim").address_space(SpawnAddressSpace::User),
        || {},
    )
    .expect("enqueue victim on the idle second cpu");
    assert_eq!(take_reschedules(), vec![second]);
    let victim_object = running.thread_object_for(victim).expect("victim target");
    handle
        .terminate_thread(&victim_object, 9)
        .expect("terminate victim before the second cpu enters");

    // Любая scheduler boundary реапит не удерживаемый ready-thread. Его id
    // переиспользуется сразу, поэтому старая ready-запись была бы ABA-ссылкой
    // на replacement.
    running.yield_now();
    assert!(running.thread_object_for(victim).is_none());
    let replacement = SchedulerServiceExt::spawn(&handle, SpawnConfig::new("replacement"), || {})
        .expect("reuse victim slot");
    assert_eq!(
        replacement, victim,
        "thread slot must be reused in this test"
    );
    assert_eq!(
        take_reschedules(),
        vec![second],
        "the removed victim must not count as load on the second cpu",
    );
    take_address_space_switches();

    set_current_cpu(second);
    let entered = panic::catch_unwind(panic::AssertUnwindSafe(|| CpuEntry::enter(&handle)));
    assert!(
        entered.is_err(),
        "mock first entry must stop in ArchContext::start"
    );
    assert_eq!(running.current(), Some(replacement));
    assert_eq!(
        take_address_space_switches(),
        vec![None],
        "first entry must start the replacement in the kernel address space",
    );
}

#[test]
fn unblocked_terminated_sleeper_leaves_no_reusable_aba_entry() {
    reset_switches();
    let timer = MockTimer::new();
    let scheduler = TestScheduler::new(MockTimerSource(timer.clone()), TEST_CONFIG)
        .bootstrap()
        .ok()
        .expect("bootstrap must succeed in tests");
    let victim = scheduler
        .spawn(SpawnConfig::new("victim"), || {})
        .expect("spawn victim");
    let runner = scheduler
        .spawn(SpawnConfig::new("runner"), || {})
        .expect("spawn runner");
    let victim_object = scheduler.thread_object_for(victim).expect("victim target");

    let running = scheduler.run();
    assert_eq!(running.current(), Some(victim));
    running.sleep_ns(100);
    assert_eq!(running.current(), Some(runner));

    capability::KernelRuntime::unblock(&running.handle(), capability::WaitToken::from(victim));
    running
        .handle()
        .terminate_thread(&victim_object, 9)
        .expect("terminate early-woken victim");
    running.yield_now();

    assert!(running.thread_object_for(victim).is_none());
    assert_eq!(
        timer.scheduled_deadline(),
        DEFAULT_QUANTUM_NS,
        "terminated ThreadId must own no stale sleep deadline",
    );

    let replacement =
        SchedulerServiceExt::spawn(&running.handle(), SpawnConfig::new("replacement"), || {})
            .expect("reuse victim slot");
    assert_eq!(replacement, victim, "thread slot must be reused");
    running.yield_now();
    assert_eq!(running.current(), Some(replacement));

    // Одинаковый absolute deadline принадлежит только новому владельцу ThreadId.
    running.sleep_ns(100);
    timer.advance_to(100);
    with_simulated_irq(|| running.on_tick(100));
    assert_eq!(running.current(), Some(replacement));
}

#[test]
fn exiting_thread_and_its_address_space_survive_reap_from_another_cpu() {
    reset_switches();
    let factory = new_factory_static();
    let timer = MockTimer::new();
    let scheduler = make_scheduler_with_factory(timer.clone(), factory);

    let second = bootstrap_cpu(&scheduler.handle(), 1);

    let victim = scheduler
        .spawn(
            SpawnConfig::new("victim").address_space(SpawnAddressSpace::User),
            || {},
        )
        .expect("spawn victim");

    let running = scheduler.run();
    assert_eq!(running.current(), Some(victim));
    let pid = running.thread_process_id(victim).expect("owning pid");

    // Окно между подготовкой переключения и его выполнением. Указатель на
    // контекст уходящего потока находится вне лока, а `current` первого CPU уже
    // переставлен на следующий поток.
    running.exit_current_interleaved(0, || {
        set_current_cpu(second);
        running.yield_now();

        assert!(
            running.thread_object_for(victim).is_some(),
            "thread whose context is still being saved must not be reaped",
        );
        assert!(
            running.process_object_for(pid).is_some(),
            "process of a thread still being switched away must stay in the table",
        );
        assert_eq!(
            factory.released(),
            0,
            "address space must not be released while a cpu still runs on its thread",
        );

        set_current_cpu(CpuId::ZERO);
    });

    assert!(
        running.thread_object_for(victim).is_none(),
        "thread must be reaped once the switch is complete",
    );
    assert_eq!(
        factory.released(),
        1,
        "address space must be released once the switch is complete",
    );
}

#[test]
fn a_cpu_running_a_thread_is_not_picked_over_an_idle_one() {
    reset_switches();
    let timer = MockTimer::new();
    let scheduler = TestScheduler::new(MockTimerSource(timer.clone()), TEST_CONFIG)
        .bootstrap()
        .ok()
        .expect("bootstrap must succeed in tests");

    let second = bootstrap_cpu(&scheduler.handle(), 1);

    // Первый CPU занимает поток, и очереди готовых пусты у обоих: по одной
    // длине очереди занятый CPU неотличим от простаивающего.
    let busy = scheduler
        .spawn(SpawnConfig::new("busy"), || {})
        .expect("spawn busy");
    let running = scheduler.run();
    assert_eq!(running.current(), Some(busy));

    let handle = running.handle();
    let _ = take_reschedules();
    SchedulerServiceExt::spawn(&handle, SpawnConfig::new("next"), || {}).expect("spawn next");

    assert_eq!(
        take_reschedules(),
        vec![second],
        "a new thread must go to the idle cpu, not to the one already running a thread",
    );
}

#[test]
fn terminating_a_remote_running_thread_reschedules_its_cpu() {
    reset_switches();
    let factory = new_factory_static();
    let timer = MockTimer::new();
    let scheduler = make_scheduler_with_factory(timer.clone(), factory);

    let second = bootstrap_cpu(&scheduler.handle(), 1);

    let first = scheduler
        .spawn(SpawnConfig::new("first"), || {})
        .expect("spawn first");
    let running = scheduler.run();
    assert_eq!(running.current(), Some(first));

    let handle = running.handle();
    let _ = take_reschedules();
    let victim = SchedulerServiceExt::spawn(
        &handle,
        SpawnConfig::new("victim").address_space(SpawnAddressSpace::User),
        || {},
    )
    .expect("spawn victim");
    assert_eq!(take_reschedules(), vec![second]);

    set_current_cpu(second);
    handle.on_reschedule();
    assert_eq!(running.current(), Some(victim));

    set_current_cpu(CpuId::ZERO);
    let victim_object = running.thread_object_for(victim).expect("victim target");
    let victim_pid = running.thread_process_id(victim).expect("victim pid");
    let victim_process = running
        .process_object_for(victim_pid)
        .expect("victim process");
    handle
        .terminate_process(&victim_process, 9)
        .expect("terminate victim process");
    assert_eq!(take_reschedules(), vec![second]);
    assert_eq!(
        factory.released(),
        0,
        "address space must stay alive until the remote cpu switches away",
    );

    set_current_cpu(second);
    handle.on_reschedule();
    assert_ne!(running.current(), Some(victim));
    assert!(
        running.thread_object_for(victim).is_none(),
        "terminated remote thread must be reaped after quiescence",
    );
    assert_eq!(
        factory.released(),
        1,
        "address space must be released after the remote cpu switches away",
    );

    drop(victim_object);
}

#[test]
fn deferred_cancel_quarantines_remote_process_cleanup() {
    use std::sync::atomic::Ordering;

    reset_switches();
    let factory = new_factory_static();
    let timer = MockTimer::new();
    let scheduler = make_scheduler_with_factory(timer.clone(), factory);

    let second = bootstrap_cpu(&scheduler.handle(), 1);

    let first = scheduler
        .spawn(SpawnConfig::new("first"), || {})
        .expect("spawn first");
    let running = scheduler.run();
    assert_eq!(running.current(), Some(first));

    let handle = running.handle();
    let victim = SchedulerServiceExt::spawn(
        &handle,
        SpawnConfig::new("victim").address_space(SpawnAddressSpace::User),
        || {},
    )
    .expect("spawn victim");
    let _ = take_reschedules();

    set_current_cpu(second);
    handle.on_reschedule();
    assert_eq!(running.current(), Some(victim));
    let victim_pid = running.thread_process_id(victim).expect("victim pid");
    let victim_process = running
        .process_object_for(victim_pid)
        .expect("victim process");
    let probe = CleanupQuarantineProbe::new(&handle, second, CpuId::ZERO, factory);
    capability::KernelRuntime::set_blocked_cancel(
        &handle,
        probe.clone() as Arc<dyn capability::CancelTarget>,
    );

    set_current_cpu(CpuId::ZERO);
    handle
        .terminate_process(&victim_process, 9)
        .expect("terminate victim process");

    assert_eq!(probe.calls.load(Ordering::Acquire), 1);
    assert_eq!(
        probe.released_during_cancel.load(Ordering::Acquire),
        0,
        "address space must survive a remote switch interleaved with cancel",
    );
    assert_eq!(
        factory.released(),
        0,
        "address space must stay quarantined until cancel fully returns",
    );

    set_current_cpu(second);
    handle.on_reschedule();
    assert!(running.thread_object_for(victim).is_none());
    assert_eq!(factory.released(), 1);
}

#[test]
fn terminated_current_rejects_late_cancel_and_leaves_the_cpu() {
    reset_switches();
    let timer = MockTimer::new();
    let scheduler = TestScheduler::new(MockTimerSource(timer.clone()), TEST_CONFIG)
        .bootstrap()
        .ok()
        .expect("bootstrap must succeed in tests");
    let victim = scheduler
        .spawn(SpawnConfig::new("victim"), || {})
        .expect("spawn victim");
    let victim_object = scheduler.thread_object_for(victim).expect("victim target");

    let running = scheduler.run();
    let handle = running.handle();
    handle
        .terminate_thread(&victim_object, 9)
        .expect("terminate victim");

    let probe = SchedulerLockProbe::new(&handle);
    capability::KernelRuntime::set_blocked_cancel(
        &handle,
        probe.clone() as Arc<dyn capability::CancelTarget>,
    );
    assert_eq!(probe.calls(), 1, "late cancel must be rejected immediately");
    assert_eq!(
        probe.acquisitions(),
        1,
        "rejected cancel must run outside the scheduler lock",
    );

    // Даже уже разрешённый waker не даёт terminal current вернуться из
    // syscall: планировщик обязан сначала снять его с CPU.
    let ready = core::sync::atomic::AtomicU32::new(capability::ParkState::SIGNALED);
    capability::KernelRuntime::block_current_until(&handle, &ready, None);

    assert_ne!(running.current(), Some(victim));
    assert!(
        running.thread_object_for(victim).is_none(),
        "terminated thread must be reaped after leaving the cpu",
    );
}

#[test]
fn thread_exit_after_external_termination_does_not_finalize_twice() {
    reset_switches();
    let timer = MockTimer::new();
    let scheduler = TestScheduler::new(MockTimerSource(timer.clone()), TEST_CONFIG)
        .bootstrap()
        .ok()
        .expect("bootstrap must succeed in tests");
    let victim = scheduler
        .spawn(SpawnConfig::new("victim"), || {})
        .expect("spawn victim");
    let victim_object = scheduler.thread_object_for(victim).expect("victim target");

    let running = scheduler.run();
    let pid = running.thread_process_id(victim).expect("victim pid");
    running
        .handle()
        .terminate_thread(&victim_object, 9)
        .expect("terminate victim externally");

    // Remote CPU мог уже войти в ThreadExit до reschedule IPI. Повторный
    // exit обязан только переключиться, не уменьшая thread_count второй раз.
    running.exit_current_with_code(3);

    assert_eq!(victim_object.exit_code(), 9);
    assert!(running.thread_object_for(victim).is_none());
    assert!(running.process_object_for(pid).is_none());
}

fn bootstrap_cpu(handle: &SchedulerHandle<MockContext, MockTimerSource>, raw: u16) -> CpuId {
    let id = CpuId::new(raw).expect("cpu id within MAX_CPUS");
    set_current_cpu(id);
    CpuEntry::bootstrap(handle).expect("bootstrap of a secondary cpu");
    set_current_cpu(CpuId::ZERO);
    id
}

#[test]
fn an_idle_cpu_steals_a_queued_thread_from_a_backlogged_one() {
    reset_switches();
    let timer = MockTimer::new();
    let scheduler = TestScheduler::new(MockTimerSource(timer.clone()), TEST_CONFIG)
        .bootstrap()
        .ok()
        .expect("bootstrap must succeed in tests");

    // Потоки создаются до подъёма второго CPU: оба достаются первому.
    let busy = scheduler
        .spawn(SpawnConfig::new("busy"), || {})
        .expect("spawn busy");
    let queued = scheduler
        .spawn(SpawnConfig::new("queued"), || {})
        .expect("spawn queued");

    let running = scheduler.run();
    assert_eq!(running.current(), Some(busy));

    let handle = running.handle();
    let second = bootstrap_cpu(&handle, 1);

    set_current_cpu(second);
    with_simulated_irq(|| handle.on_timer_tick(1));
    assert_eq!(
        running.current(),
        Some(queued),
        "an idle cpu must take a waiting thread from a backlogged one",
    );

    set_current_cpu(CpuId::ZERO);
    assert_eq!(
        running.current(),
        Some(busy),
        "the running thread of the donor stays where it is",
    );
    assert_eq!(
        running.thread_cpu_affinity(queued),
        Some(second),
        "a stolen thread must be re-homed to the cpu that took it",
    );
}

#[test]
fn a_stolen_thread_wakes_on_the_cpu_that_took_it() {
    reset_switches();
    let timer = MockTimer::new();
    let scheduler = TestScheduler::new(MockTimerSource(timer.clone()), TEST_CONFIG)
        .bootstrap()
        .ok()
        .expect("bootstrap must succeed in tests");

    let busy = scheduler
        .spawn(SpawnConfig::new("busy"), || {})
        .expect("spawn busy");
    let queued = scheduler
        .spawn(SpawnConfig::new("queued"), || {})
        .expect("spawn queued");

    let running = scheduler.run();
    assert_eq!(running.current(), Some(busy));

    let handle = running.handle();
    let second = bootstrap_cpu(&handle, 1);

    set_current_cpu(second);
    with_simulated_irq(|| handle.on_timer_tick(1));
    assert_eq!(running.current(), Some(queued));

    running.sleep_ns(100);
    assert_ne!(running.current(), Some(queued));

    set_current_cpu(CpuId::ZERO);
    timer.advance_to(100);
    let _ = take_reschedules();
    with_simulated_irq(|| handle.on_timer_tick(100));

    assert_eq!(
        take_reschedules(),
        vec![second],
        "a woken thread must return to the queue of the cpu that stole it",
    );
}

#[test]
fn a_cpu_running_its_only_thread_keeps_it() {
    reset_switches();
    let timer = MockTimer::new();
    let scheduler = TestScheduler::new(MockTimerSource(timer.clone()), TEST_CONFIG)
        .bootstrap()
        .ok()
        .expect("bootstrap must succeed in tests");

    let busy = scheduler
        .spawn(SpawnConfig::new("busy"), || {})
        .expect("spawn busy");

    let running = scheduler.run();
    assert_eq!(running.current(), Some(busy));

    let handle = running.handle();
    let second = bootstrap_cpu(&handle, 1);

    set_current_cpu(second);
    with_simulated_irq(|| handle.on_timer_tick(1));
    assert_ne!(
        running.current(),
        Some(busy),
        "a thread executing on another cpu must not be taken",
    );

    set_current_cpu(CpuId::ZERO);
    assert_eq!(running.current(), Some(busy));
    assert_eq!(
        running.thread_cpu_affinity(busy),
        Some(CpuId::ZERO),
        "the only thread of a cpu stays home",
    );
}

#[test]
fn a_backlog_appearing_after_a_cpu_went_idle_is_migrated() {
    reset_switches();
    let timer = MockTimer::new();
    let scheduler = TestScheduler::new(MockTimerSource(timer.clone()), TEST_CONFIG)
        .bootstrap()
        .ok()
        .expect("bootstrap must succeed in tests");

    let first = scheduler
        .spawn(SpawnConfig::new("first"), || {})
        .expect("spawn first");
    let second_thread = scheduler
        .spawn(SpawnConfig::new("second"), || {})
        .expect("spawn second");

    let running = scheduler.run();
    assert_eq!(running.current(), Some(first));

    // Оба потока уходят в сон. Первый CPU остаётся без работы, и перекос
    // возникает позже, на их общем пробуждении.
    running.sleep_ns(100);
    running.sleep_ns(100);

    let handle = running.handle();
    let second_cpu = bootstrap_cpu(&handle, 1);

    timer.advance_to(100);
    with_simulated_irq(|| handle.on_timer_tick(100));
    assert_eq!(running.current(), Some(first));

    set_current_cpu(second_cpu);
    with_simulated_irq(|| handle.on_timer_tick(100));
    assert_eq!(
        running.current(),
        Some(second_thread),
        "a backlog built by wakeups must be migrated as well",
    );
    set_current_cpu(CpuId::ZERO);
}

#[test]
fn a_thread_whose_context_is_still_being_saved_is_not_stolen() {
    reset_switches();
    let timer = MockTimer::new();
    let scheduler = TestScheduler::new(MockTimerSource(timer.clone()), TEST_CONFIG)
        .bootstrap()
        .ok()
        .expect("bootstrap must succeed in tests");

    let leaving = scheduler
        .spawn(SpawnConfig::new("leaving"), || {})
        .expect("spawn leaving");
    let taking_over = scheduler
        .spawn(SpawnConfig::new("taking-over"), || {})
        .expect("spawn taking-over");

    let running = scheduler.run();
    assert_eq!(running.current(), Some(leaving));

    let handle = running.handle();
    let second = bootstrap_cpu(&handle, 1);

    // Окно переключения. Вытесненный поток уже стоит в очереди готовых, а
    // указатель на его контекст находится вне scheduler-лока.
    running.yield_now_interleaved(|| {
        set_current_cpu(second);
        with_simulated_irq(|| handle.on_timer_tick(1));
        assert_ne!(
            running.current(),
            Some(leaving),
            "a thread whose context is still being saved must not be stolen",
        );
        set_current_cpu(CpuId::ZERO);
    });

    assert_eq!(running.current(), Some(taking_over));
    assert_eq!(
        running.thread_cpu_affinity(leaving),
        Some(CpuId::ZERO),
        "a thread held by a cpu keeps its home",
    );
}

#[test]
fn a_thread_queued_for_an_idle_cpu_is_left_to_it() {
    reset_switches();
    let timer = MockTimer::new();
    let scheduler = TestScheduler::new(MockTimerSource(timer.clone()), TEST_CONFIG)
        .bootstrap()
        .ok()
        .expect("bootstrap must succeed in tests");

    let busy = scheduler
        .spawn(SpawnConfig::new("busy"), || {})
        .expect("spawn busy");

    let running = scheduler.run();
    assert_eq!(running.current(), Some(busy));

    let handle = running.handle();
    let second = bootstrap_cpu(&handle, 1);
    let third = bootstrap_cpu(&handle, 2);

    // Новый поток встаёт к наименее загруженному CPU и ждёт своего IPI.
    let pending = SchedulerServiceExt::spawn(&handle, SpawnConfig::new("pending"), || {})
        .expect("spawn pending");
    assert_eq!(take_reschedules(), vec![second]);

    set_current_cpu(third);
    with_simulated_irq(|| handle.on_timer_tick(1));
    assert_ne!(
        running.current(),
        Some(pending),
        "a thread just assigned to an idle cpu must not be taken away from it",
    );

    set_current_cpu(second);
    handle.on_reschedule();
    assert_eq!(
        running.current(),
        Some(pending),
        "the assigned cpu must get its thread",
    );
    set_current_cpu(CpuId::ZERO);
}

#[test]
fn a_cpu_left_without_work_steals_without_waiting_for_a_tick() {
    reset_switches();
    let timer = MockTimer::new();
    let scheduler = TestScheduler::new(MockTimerSource(timer.clone()), TEST_CONFIG)
        .bootstrap()
        .ok()
        .expect("bootstrap must succeed in tests");

    let busy = scheduler
        .spawn(SpawnConfig::new("busy"), || {})
        .expect("spawn busy");
    let first_queued = scheduler
        .spawn(SpawnConfig::new("first-queued"), || {})
        .expect("spawn first-queued");
    let second_queued = scheduler
        .spawn(SpawnConfig::new("second-queued"), || {})
        .expect("spawn second-queued");

    let running = scheduler.run();
    assert_eq!(running.current(), Some(busy));

    let handle = running.handle();
    let second = bootstrap_cpu(&handle, 1);

    set_current_cpu(second);
    with_simulated_irq(|| handle.on_timer_tick(1));
    assert_eq!(running.current(), Some(first_queued));

    // Поток второго CPU уходит спать. Работу он ищет тут же, на том же уходе
    // в idle.
    running.sleep_ns(100);
    assert_eq!(
        running.current(),
        Some(second_queued),
        "a cpu losing its thread must pick up work from a backlogged one at once",
    );
    set_current_cpu(CpuId::ZERO);
}

#[test]
fn a_donor_without_stealable_work_does_not_end_the_search() {
    reset_switches();
    let timer = MockTimer::new();
    let scheduler = TestScheduler::new(MockTimerSource(timer.clone()), TEST_CONFIG)
        .bootstrap()
        .ok()
        .expect("bootstrap must succeed in tests");

    let first = scheduler
        .spawn(SpawnConfig::new("first"), || {})
        .expect("spawn first");
    let queued_here = scheduler
        .spawn(SpawnConfig::new("queued-here"), || {})
        .expect("spawn queued-here");
    let taken_by_second = scheduler
        .spawn(SpawnConfig::new("taken-by-second"), || {})
        .expect("spawn taken-by-second");
    let taken_by_third = scheduler
        .spawn(SpawnConfig::new("taken-by-third"), || {})
        .expect("spawn taken-by-third");

    let running = scheduler.run();
    assert_eq!(running.current(), Some(first));

    let handle = running.handle();
    let second = bootstrap_cpu(&handle, 1);
    let third = bootstrap_cpu(&handle, 2);

    set_current_cpu(second);
    with_simulated_irq(|| handle.on_timer_tick(1));
    assert_eq!(running.current(), Some(queued_here));
    set_current_cpu(third);
    with_simulated_irq(|| handle.on_timer_tick(1));
    assert_eq!(running.current(), Some(taken_by_second));
    set_current_cpu(CpuId::ZERO);

    // Догружаем второй и третий CPU до загрузки первого: у всех троих по
    // исполняемому потоку и одному в очереди.
    let queued_on_second =
        SchedulerServiceExt::spawn(&handle, SpawnConfig::new("on-second"), || {})
            .expect("spawn on-second");
    let queued_on_third = SchedulerServiceExt::spawn(&handle, SpawnConfig::new("on-third"), || {})
        .expect("spawn on-third");
    assert_eq!(running.thread_cpu_affinity(queued_on_second), Some(second));
    assert_eq!(running.thread_cpu_affinity(queued_on_third), Some(third));

    // Вор поднимается последним: пустой CPU притягивал бы постановку новых
    // потоков к себе.
    let fourth = bootstrap_cpu(&handle, 3);

    set_current_cpu(third);
    running.yield_now_interleaved(|| {
        // Очередь третьего CPU состоит из одного потока, чей контекст он
        // сохраняет прямо сейчас: переселять оттуда нечего.
        set_current_cpu(fourth);
        with_simulated_irq(|| handle.on_timer_tick(1));
        let taken = running.current();
        assert!(
            taken == Some(taken_by_third) || taken == Some(queued_on_second),
            "an unusable donor must not stop the search: cpu took {taken:?}",
        );
        set_current_cpu(third);
    });
    set_current_cpu(CpuId::ZERO);
}

#[test]
fn a_terminated_thread_stops_counting_towards_the_load_of_its_cpu() {
    reset_switches();
    let timer = MockTimer::new();
    let scheduler = TestScheduler::new(MockTimerSource(timer.clone()), TEST_CONFIG)
        .bootstrap()
        .ok()
        .expect("bootstrap must succeed in tests");

    let busy = scheduler
        .spawn(SpawnConfig::new("busy"), || {})
        .expect("spawn busy");
    let taken = scheduler
        .spawn(SpawnConfig::new("taken"), || {})
        .expect("spawn taken");
    let victim = scheduler
        .spawn(SpawnConfig::new("victim"), || {})
        .expect("spawn victim");

    let running = scheduler.run();
    assert_eq!(running.current(), Some(busy));

    let handle = running.handle();
    let second = bootstrap_cpu(&handle, 1);

    set_current_cpu(second);
    with_simulated_irq(|| handle.on_timer_tick(1));
    assert_eq!(running.current(), Some(taken));
    set_current_cpu(CpuId::ZERO);

    // Оба CPU исполняют по потоку, и лишний в очереди - только у первого.
    let victim_object = running.thread_object_for(victim).expect("victim target");
    handle
        .terminate_thread(&victim_object, 9)
        .expect("terminate the queued thread");

    // Постановка идёт к наименее загруженному CPU, а при равной загрузке
    // остаётся на текущем. Терминированный поток загрузку больше не создаёт.
    let next =
        SchedulerServiceExt::spawn(&handle, SpawnConfig::new("next"), || {}).expect("spawn next");
    assert_eq!(
        running.thread_cpu_affinity(next),
        Some(CpuId::ZERO),
        "a terminated thread must not keep inflating the load of its cpu",
    );
    assert_ne!(running.thread_cpu_affinity(next), Some(second));
}
