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
    Priority, Scheduler, SchedulerConfig, SchedulerHandle, SpawnAddressSpace, SpawnConfig,
    SpawnError, Uninit,
};

use crate::common::{
    CountingFrameAllocator, MockAddressSpaceFactory, MockContext, MockTimer, MockTimerSource,
    preemption_enabled, reset_switches, switch_count, take_address_space_switches,
    with_simulated_irq,
};

type TestScheduler = Scheduler<MockContext, MockTimerSource, Uninit>;
type SmallScheduler = Scheduler<MockContext, MockTimerSource, Uninit>;

const TEST_CONFIG: SchedulerConfig = SchedulerConfig::new(32, 16);
const SMALL_CONFIG: SchedulerConfig = SchedulerConfig::new(4, 8);

struct ForwardingRuntime {
    inner: RwLock<Option<Arc<dyn kobject::KernelRuntime>>>,
}

impl ForwardingRuntime {
    fn set(&self, rt: Arc<dyn kobject::KernelRuntime>) {
        *self.inner.write().unwrap() = Some(rt);
    }

    fn clear(&self) {
        *self.inner.write().unwrap() = None;
    }

    fn with<R>(&self, f: impl FnOnce(&dyn kobject::KernelRuntime) -> R) -> R {
        let guard = self.inner.read().unwrap();
        let rt = guard
            .as_ref()
            .expect("ForwardingRuntime: inner runtime not set");
        f(rt.as_ref())
    }
}

impl kobject::KernelRuntime for ForwardingRuntime {
    fn current_wait_token(&self) -> kobject::WaitToken {
        self.with(|rt| rt.current_wait_token())
    }

    fn current_handle_table(&self) -> Option<Arc<collections::MutexCell<kobject::HandleTable>>> {
        self.with(|rt| rt.current_handle_table())
    }

    fn exit_current_thread(&self, exit_code: i32) -> ! {
        self.with(|rt| rt.exit_current_thread(exit_code))
    }

    fn block_current_until(
        &self,
        ready_flag: &core::sync::atomic::AtomicU32,
        timeout_ns: Option<u64>,
    ) {
        self.with(|rt| rt.block_current_until(ready_flag, timeout_ns));
    }

    fn unblock(&self, token: kobject::WaitToken) {
        self.with(|rt| rt.unblock(token));
    }

    fn set_blocked_cancel(&self, cancel: Arc<dyn kobject::CancelTarget>) {
        self.with(|rt| rt.set_blocked_cancel(cancel));
    }

    fn clear_blocked_cancel(&self) {
        self.with(|rt| rt.clear_blocked_cancel());
    }
}

fn install_forwarding_runtime() -> Arc<ForwardingRuntime> {
    static FORWARDER: OnceLock<Arc<ForwardingRuntime>> = OnceLock::new();
    static INSTALLED: Once = Once::new();

    let _ = kobject::ParkState::REGISTERED;
    let forwarder = FORWARDER
        .get_or_init(|| {
            Arc::new(ForwardingRuntime {
                inner: RwLock::new(None),
            })
        })
        .clone();
    INSTALLED.call_once(|| {
        let dyn_rt: Arc<dyn kobject::KernelRuntime> = forwarder.clone();
        kobject::install_runtime(dyn_rt);
    });
    forwarder
}

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
    assert_eq!(timer.scheduled_deadline(), scheduler::DEFAULT_QUANTUM_NS);
    assert_eq!(switch_count(), 1);
}

/// Точки входа preemption, ротирующие равноприоритетные потоки round-robin.
/// Параметризуем тест по способу уступки кванта (time-slice-тик vs явный yield),
/// чтобы не плодить три почти идентичных теста на один и тот же контракт.
enum PreemptKind {
    OnTick,
    Yield,
}

fn round_robin_via(kind: &PreemptKind) {
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
    assert_eq!(running.current(), second);

    step(&running);
    assert_eq!(running.current(), first);
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
    with_simulated_irq(|| running.on_tick(50));
    assert_eq!(running.current(), sleeper);
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
    assert_eq!(switch_count(), switches_before);
}

#[test]
fn invalid_priority_returns_error() {
    let timer = MockTimer::new();
    let scheduler = SmallScheduler::new(MockTimerSource(timer), SMALL_CONFIG).bootstrap();
    let err = scheduler
        .spawn(SpawnConfig::new("bad").priority(Priority::new(10)), || {})
        .unwrap_err();
    assert_eq!(err, SpawnError::InvalidPriority);
}

#[test]
fn zero_stack_pages_returns_invalid_stack_pages_error() {
    let timer = MockTimer::new();
    let scheduler = TestScheduler::new(MockTimerSource(timer), TEST_CONFIG).bootstrap();
    let err = scheduler
        .spawn(SpawnConfig::new("nostack").stack_pages(0), || {})
        .unwrap_err();
    assert_eq!(err, SpawnError::InvalidStackPages);
}

#[test]
fn spawn_user_thread_without_factory_fails_with_address_space_creation_failed() {
    let timer = MockTimer::new();
    let scheduler = TestScheduler::new(MockTimerSource(timer), TEST_CONFIG).bootstrap();
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
    let scheduler = SmallScheduler::new(MockTimerSource(timer.clone()), SMALL_CONFIG).bootstrap();
    let task = scheduler
        .spawn(SpawnConfig::new("task").priority(Priority::new(1)), || {})
        .unwrap();
    let running = scheduler.run();
    assert_eq!(running.current(), task);
}

#[test]
fn yield_restores_preemption_when_enabled() {
    reset_switches();
    let timer = MockTimer::new();
    let scheduler = TestScheduler::new(MockTimerSource(timer.clone()), TEST_CONFIG).bootstrap();
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
    let scheduler = TestScheduler::new(MockTimerSource(timer.clone()), TEST_CONFIG).bootstrap();
    let _ = scheduler.spawn(SpawnConfig::new("a"), || {}).unwrap();
    let _ = scheduler.spawn(SpawnConfig::new("b"), || {}).unwrap();

    let running = scheduler.run();
    with_simulated_irq(|| {
        running.yield_now();
        assert!(
            !preemption_enabled(),
            "вложенный yield не должен разрешать preemption"
        );
    });
    assert!(
        preemption_enabled(),
        "preemption восстановлен после masked-scope"
    );
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
    with_simulated_irq(|| running.on_tick(50));
    assert_eq!(running.current(), late);

    timer.advance_to(100);
    with_simulated_irq(|| running.on_tick(100));
    let after_second_tick = running.current();
    assert!(
        after_second_tick == runner || after_second_tick == early || after_second_tick == late,
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
            .bootstrap();
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

fn new_factory_static() -> &'static MockAddressSpaceFactory {
    Box::leak(Box::new(MockAddressSpaceFactory::new()))
}

fn mark_process_loaded(
    handle: &SchedulerHandle<MockContext, MockTimerSource>,
    process: &Arc<kobject::ProcessObject>,
) {
    let install = kobject::UserImageInstall {
        segments: std::vec::Vec::new(),
        entry: VirtualAddress::new(0x4000_0000),
        user_stack_top: VirtualAddress::new(0x5000_1000),
        user_stack_size: 0x1000,
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
}

#[test]
fn switch_between_threads_in_same_process_does_not_change_address_space() {
    reset_switches();
    let timer = MockTimer::new();
    let scheduler = TestScheduler::new(MockTimerSource(timer.clone()), TEST_CONFIG).bootstrap();
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
    assert_eq!(running.current(), u);
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
    let scheduler = TestScheduler::new(MockTimerSource(timer.clone()), TEST_CONFIG).bootstrap();
    let id = scheduler
        .spawn(SpawnConfig::new("t"), || {})
        .expect("spawn t");
    let ko = scheduler.thread_object_for(id).expect("thread_object");

    let running = scheduler.run();
    assert!(!ko.terminated());

    running.exit_current();

    assert!(ko.terminated());
    assert_eq!(ko.exit_code(), 0);
}

#[test]
fn exit_current_with_nonzero_code_publishes_code() {
    reset_switches();
    let timer = MockTimer::new();
    let scheduler = TestScheduler::new(MockTimerSource(timer.clone()), TEST_CONFIG).bootstrap();
    let id = scheduler
        .spawn(SpawnConfig::new("t"), || {})
        .expect("spawn t");
    let ko = scheduler.thread_object_for(id).expect("thread_object");

    let running = scheduler.run();
    running.exit_current_with_code(42);

    assert!(ko.terminated());
    assert_eq!(ko.exit_code(), 42);
}

#[test]
fn last_thread_exit_signals_process_terminated() {
    reset_switches();
    let timer = MockTimer::new();
    let scheduler = TestScheduler::new(MockTimerSource(timer.clone()), TEST_CONFIG).bootstrap();
    let id = scheduler
        .spawn(SpawnConfig::new("solo"), || {})
        .expect("spawn solo");
    let pid = scheduler.thread_process_id(id).expect("process id");
    let process_ko = scheduler.process_object_for(pid).expect("process_object");

    let running = scheduler.run();
    assert!(!process_ko.terminated());

    running.exit_current();

    assert!(process_ko.terminated());
    assert_eq!(process_ko.exit_code(), 0);
}

#[test]
fn non_last_thread_exit_does_not_signal_process() {
    use scheduler::SchedulerService;

    reset_switches();
    let timer = MockTimer::new();
    let scheduler = TestScheduler::new(MockTimerSource(timer.clone()), TEST_CONFIG).bootstrap();
    let first = scheduler
        .spawn(SpawnConfig::new("first"), || {})
        .expect("spawn first");

    let running = scheduler.run();
    assert_eq!(running.current(), first);
    let pid = running.thread_process_id(first).expect("process id");

    let second = running
        .handle()
        .spawn_boxed(
            SpawnConfig::new("second").address_space(SpawnAddressSpace::Inherit),
            Box::new(|| {}),
        )
        .expect("spawn second");
    assert_eq!(running.thread_process_id(second), Some(pid));

    let process_ko = running.process_object_for(pid).expect("process_object");
    let first_ko = running.thread_object_for(first).expect("first ko");

    running.exit_current();

    assert!(first_ko.terminated());
    assert!(!process_ko.terminated());
    assert!(running.process_object_for(pid).is_some());
}

#[test]
fn process_object_outlives_process_table_entry() {
    reset_switches();
    let timer = MockTimer::new();
    let scheduler = TestScheduler::new(MockTimerSource(timer.clone()), TEST_CONFIG).bootstrap();
    let id = scheduler
        .spawn(SpawnConfig::new("zombie"), || {})
        .expect("spawn");
    let pid = scheduler.thread_process_id(id).expect("process id");
    let process_ko = scheduler.process_object_for(pid).expect("process_object");

    let count_before = scheduler.process_count();
    let running = scheduler.run();
    running.exit_current();

    assert!(running.process_object_for(pid).is_none());
    assert!(running.process_count() < count_before);
    assert!(Arc::strong_count(&process_ko) >= 1);
    assert!(process_ko.terminated());
}

#[test]
fn current_thread_object_returns_running_thread_ko() {

    reset_switches();
    let timer = MockTimer::new();
    let scheduler = TestScheduler::new(MockTimerSource(timer.clone()), TEST_CONFIG).bootstrap();
    let id = scheduler
        .spawn(SpawnConfig::new("t"), || {})
        .expect("spawn t");
    let expected = scheduler.thread_object_for(id).expect("thread_object");

    let running = scheduler.run();
    assert_eq!(running.current(), id);

    let observed = running
        .handle()
        .current_thread_object()
        .expect("current_thread_object");
    assert!(Arc::ptr_eq(&observed, &expected));
}

#[test]
fn current_process_object_returns_running_process_ko() {

    reset_switches();
    let timer = MockTimer::new();
    let scheduler = TestScheduler::new(MockTimerSource(timer.clone()), TEST_CONFIG).bootstrap();
    let id = scheduler
        .spawn(SpawnConfig::new("t"), || {})
        .expect("spawn t");
    let pid = scheduler.thread_process_id(id).expect("process id");
    let expected = scheduler.process_object_for(pid).expect("process_object");

    let running = scheduler.run();
    assert_eq!(running.current(), id);

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
    let scheduler = TestScheduler::new(MockTimerSource(timer.clone()), TEST_CONFIG).bootstrap();
    let id = scheduler
        .spawn(SpawnConfig::new("t"), || {})
        .expect("spawn t");
    let ko = scheduler.thread_object_for(id).expect("thread_object");
    let handle = scheduler.handle();

    let running = scheduler.run();
    assert_eq!(running.current(), id);
    assert!(!ko.terminated());

    forwarder.set(Arc::new(handle));

    let prev_hook = panic::take_hook();
    panic::set_hook(Box::new(|_| {}));
    let result = panic::catch_unwind(panic::AssertUnwindSafe(|| kobject::thread_exit(0)));
    panic::set_hook(prev_hook);
    assert!(
        result.is_err(),
        "thread_exit must not return when scheduler::switch is a no-op"
    );

    forwarder.clear();

    assert!(ko.terminated());
    assert_eq!(ko.exit_code(), 0);
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
    assert_eq!(err, kobject::SpawnError::InvalidName);
    assert_eq!(scheduler.process_count(), processes_before);
}

#[test]
fn thread_terminate_via_handle_signals_terminated() {
    use kobject::UserThreadEntry;

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
    use kobject::UserThreadEntry;

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
    let scheduler = TestScheduler::new(MockTimerSource(timer.clone()), TEST_CONFIG).bootstrap();
    let t1 = scheduler
        .spawn(SpawnConfig::new("t1"), || {})
        .expect("spawn t1");
    let t2 = scheduler
        .spawn(SpawnConfig::new("t2"), || {})
        .expect("spawn t2");
    let t3 = scheduler
        .spawn(SpawnConfig::new("t3"), || {})
        .expect("spawn t3");
    let t2_ko = scheduler.thread_object_for(t2).expect("t2 ko");

    let running = scheduler.run();
    assert_eq!(running.current(), t1);

    let handle = running.handle();
    handle.terminate_thread(&t2_ko, 9).expect("terminate t2");
    assert!(t2_ko.terminated());

    running.yield_now();
    assert_eq!(running.current(), t3);
}

#[test]
fn spawn_rollback_undoes_inherit_increment_on_thread_table_full() {
    use scheduler::SchedulerService;

    reset_switches();
    let timer = MockTimer::new();
    let scheduler = TestScheduler::new(MockTimerSource(timer.clone()), TEST_CONFIG).bootstrap();
    let t1 = scheduler
        .spawn(SpawnConfig::new("t1"), || {})
        .expect("spawn t1");

    let running = scheduler.run();
    assert_eq!(running.current(), t1);
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
    use kobject::UserThreadEntry;

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
        Err(kobject::SpawnError::ImageNotLoaded) => {}
        other => panic!("expected ImageNotLoaded, got {:?}", other.err()),
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
        kobject::UserThreadEntry {
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
    use kobject::LoadImageError;

    reset_switches();
    let factory = new_factory_static();
    let timer = MockTimer::new();
    let scheduler = make_scheduler_with_factory(timer, factory);
    let handle = scheduler.handle();

    let process = handle
        .create_empty_process("p")
        .expect("create_empty_process");
    mark_process_loaded(&handle, &process);

    let install = kobject::UserImageInstall {
        segments: std::vec::Vec::new(),
        entry: VirtualAddress::new(0x4000_0000),
        user_stack_top: VirtualAddress::new(0x5000_1000),
        user_stack_size: 0x1000,
        user_vm_base: PageAlignedVirtualAddress::from_usize(0x4000_0000).expect("aligned"),
        user_vm_size: 0x100_0000,
    };
    match handle.load_user_image_into(&process, &install) {
        Err(LoadImageError::WrongState) => {}
        other => panic!("expected WrongState, got {other:?}"),
    }
}

fn empty_loader_table() -> Arc<collections::MutexCell<kobject::HandleTable>> {
    Arc::new(collections::MutexCell::new(kobject::HandleTable::new()))
}

#[test]
fn start_user_process_creates_thread_and_marks_loader_state() {
    use kobject::{UserStartSpec, UserThreadEntry};

    reset_switches();
    let factory = new_factory_static();
    let timer = MockTimer::new();
    let scheduler = make_scheduler_with_factory(timer, factory);
    let handle = scheduler.handle();

    let process = handle
        .create_empty_process("p")
        .expect("create_empty_process");
    mark_process_loaded(&handle, &process);

    let spec = UserStartSpec {
        entry: UserThreadEntry {
            entry_pc: 0x4000_0000,
            user_sp: 0x4001_0000,
            arg: 0,
            priority: 1,
        },
        loader_handle_table: empty_loader_table(),
        handle_ids: std::vec::Vec::new(),
        metering_resource: None,
    };
    let thread = handle.start_user_process(&process, spec).expect("start ok");
    assert!(!thread.terminated());
}

#[test]
fn start_user_process_rejects_unloaded() {
    use kobject::{StartProcessError, UserStartSpec, UserThreadEntry};

    reset_switches();
    let factory = new_factory_static();
    let timer = MockTimer::new();
    let scheduler = make_scheduler_with_factory(timer, factory);
    let handle = scheduler.handle();

    let process = handle
        .create_empty_process("p")
        .expect("create_empty_process");

    let spec = UserStartSpec {
        entry: UserThreadEntry {
            entry_pc: 0x4000_0000,
            user_sp: 0x4001_0000,
            arg: 0,
            priority: 1,
        },
        loader_handle_table: empty_loader_table(),
        handle_ids: std::vec::Vec::new(),
        metering_resource: None,
    };
    match handle.start_user_process(&process, spec) {
        Err(StartProcessError::WrongState) => {}
        Err(other) => panic!("expected WrongState, got {other:?}"),
        Ok(_) => panic!("expected WrongState, got Ok"),
    }
}

#[test]
fn start_user_process_preserves_handles_on_spawn_failure() {
    // start_user_process при SpawnFailed обязан оставить handle нетронутым
    // в loader-table с исходным HandleId (не дропать и не менять id).
    use collections::LockCell;
    use kobject::{
        Handle, HandleTable, KObject, Rights, Signal, StartProcessError, UserStartSpec,
        UserThreadEntry,
    };
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
            .spawn_boxed(inherit_cfg, std::boxed::Box::new(|| {}))
            .expect("inherit filler spawn");
    }

    let signal = Signal::new();
    let weak = std::sync::Arc::downgrade(&signal);
    let loader_table = Arc::new(collections::MutexCell::new(HandleTable::new()));
    let h = Handle::new(KObject::Signal(signal), Rights::TRANSFER | Rights::READ);
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
        handle_ids: std::vec![handle_id],
        metering_resource: None,
    };

    match handle.start_user_process(&process, spec) {
        Err(StartProcessError::SpawnFailed(_)) => {
            assert!(weak.upgrade().is_some(), "KO must stay alive after error");
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

    let install = kobject::UserImageInstall {
        segments: std::vec![kobject::UserSegmentInstall {
            va_base: PageAlignedVirtualAddress::from_usize(0x4000_0000).unwrap(),
            mapped_size: 2 * 4096,
            region: region.clone(),
            flags: MemFlags::user_rw(),
        }],
        entry: VirtualAddress::new(0x4000_0000),
        user_stack_top: VirtualAddress::new(0x5000_1000),
        user_stack_size: 0x1000,
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
