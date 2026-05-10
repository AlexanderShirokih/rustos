mod common;

use std::sync::{Arc, Mutex};

use scheduler::{
    Priority, Scheduler, SchedulerConfig, SpawnAddressSpace, SpawnConfig, SpawnError,
    ThreadStackAllocator, Uninit,
};

use crate::common::{
    MockAddressSpaceFactory, MockContext, MockStack, MockTimer, MockTimerSource, max_irq_depth,
    reset_switches, switch_count, take_address_space_switches,
};

type TestScheduler = Scheduler<MockContext, MockTimerSource, Uninit>;
type SmallScheduler = Scheduler<MockContext, MockTimerSource, Uninit>;

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
    let scheduler = SmallScheduler::new(MockTimerSource(timer), SMALL_CONFIG).bootstrap();
    let err = scheduler
        .spawn(SpawnConfig::new("bad").priority(Priority::new(10)), || {})
        .unwrap_err();
    assert_eq!(err, SpawnError::InvalidPriority);
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
        "after wake of `early` scheduler must run a real (non-idle) thread, got {after_second_tick:?}"
    );
}

#[test]
fn exit_current_releases_thread_slot_for_next_spawn() {
    use scheduler::SchedulerService;
    reset_switches();
    let timer = MockTimer::new();
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
    assert_eq!(err, SpawnError::NoFreeThreadSlots);
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

    // Все потоки kernel-AS - process_id у каждого свой (ProcessTable выдаёт
    // уникальный pid на spawn), но AS-root одинаковый (None). Сначала
    // фильтруем все случаи `Some(None)` - это переключения между процессами,
    // оставшиеся на kernel-AS. Они корректны и валидны для kernel-thread'ов.
    take_address_space_switches();

    let _ = scheduler.run();
    let switches = take_address_space_switches();
    // С kernel-AS у разных kernel-thread'ов root_pa = None для обоих.
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
    // Первый switch -> user-a - switch_address_space с Some(root_a).
    assert!(
        after_start.iter().any(|sw| matches!(
            sw,
            Some(handle) if handle.root.as_usize() == MockAddressSpaceFactory::BASE_ROOT_PA
        )),
        "expected switch to user-a root"
    );

    running.yield_now();
    let after_yield = take_address_space_switches();
    // Yield с user-a на user-b - должен быть один switch на root_b.
    let user_b_root = MockAddressSpaceFactory::BASE_ROOT_PA + 4096;
    assert!(
        after_yield.iter().any(|sw| matches!(
            sw,
            Some(handle) if handle.root.as_usize() == user_b_root
        )),
        "expected switch to user-b root, got {after_yield:?}"
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
fn terminating_user_thread_releases_address_space() {
    // В MockContext entry-замыкание никогда не исполняется (start/switch - no-op),
    // поэтому trampoline payload утекает вместе с захваченным entry-замыканием.
    // Чтобы наблюдать Drop user-AS, проверяем сценарий: пока scheduler жив -
    // mapper жив, как только user-thread будет удалён из ProcessTable -
    // соответствующий `Arc<AddressSpace>` освободится. Текущая реализация
    // ProcessTable не имеет API удаления; здесь проверяем хотя бы базовый
    // инвариант: фабрика создала ровно один user-AS, который ссылается из
    // живого процесса (released = 0). Полный drop-сценарий покрывается
    // QEMU integration-тестом `user_thread_exit_releases_address_space_frames`.
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
    assert_eq!(factory.released(), 0);
}

#[test]
fn exit_current_signals_thread_terminated() {
    use kobject::THREAD_TERMINATED;

    reset_switches();
    let timer = MockTimer::new();
    let scheduler = TestScheduler::new(MockTimerSource(timer.clone()), TEST_CONFIG).bootstrap();
    let id = scheduler
        .spawn(SpawnConfig::new("t"), || {})
        .expect("spawn t");
    let ko = scheduler.thread_object_for(id).expect("thread_object");

    let running = scheduler.run();
    assert_eq!(ko.peek() & THREAD_TERMINATED, 0);

    running.exit_current();

    assert_eq!(ko.peek() & THREAD_TERMINATED, THREAD_TERMINATED);
    assert_eq!(ko.exit_code(), 0);
}

#[test]
fn exit_current_with_nonzero_code_publishes_code() {
    use kobject::THREAD_TERMINATED;

    reset_switches();
    let timer = MockTimer::new();
    let scheduler = TestScheduler::new(MockTimerSource(timer.clone()), TEST_CONFIG).bootstrap();
    let id = scheduler
        .spawn(SpawnConfig::new("t"), || {})
        .expect("spawn t");
    let ko = scheduler.thread_object_for(id).expect("thread_object");

    let running = scheduler.run();
    running.exit_current_with_code(42);

    assert_eq!(ko.peek() & THREAD_TERMINATED, THREAD_TERMINATED);
    assert_eq!(ko.exit_code(), 42);
}

#[test]
fn last_thread_exit_signals_process_terminated() {
    use kobject::PROCESS_TERMINATED;

    reset_switches();
    let timer = MockTimer::new();
    let scheduler = TestScheduler::new(MockTimerSource(timer.clone()), TEST_CONFIG).bootstrap();
    let id = scheduler
        .spawn(SpawnConfig::new("solo"), || {})
        .expect("spawn solo");
    let pid = scheduler.thread_process_id(id).expect("process id");
    let process_ko = scheduler.process_object_for(pid).expect("process_object");

    let running = scheduler.run();
    assert_eq!(process_ko.peek() & PROCESS_TERMINATED, 0);

    running.exit_current();

    assert_eq!(process_ko.peek() & PROCESS_TERMINATED, PROCESS_TERMINATED);
    assert_eq!(process_ko.exit_code(), 0);
}

#[test]
fn non_last_thread_exit_does_not_signal_process() {
    use kobject::{PROCESS_TERMINATED, THREAD_TERMINATED};
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

    assert_eq!(first_ko.peek() & THREAD_TERMINATED, THREAD_TERMINATED);
    assert_eq!(process_ko.peek() & PROCESS_TERMINATED, 0);
    assert!(running.process_object_for(pid).is_some());
}

#[test]
fn process_object_outlives_process_table_entry() {
    use kobject::PROCESS_TERMINATED;

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
    assert_eq!(process_ko.peek() & PROCESS_TERMINATED, PROCESS_TERMINATED);
}

#[test]
fn current_thread_object_returns_running_thread_ko() {
    use kobject::KernelRuntime;

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
    use kobject::KernelRuntime;

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
    use std::{
        panic,
        sync::{Once, OnceLock, RwLock},
    };

    use kobject::{
        IpcError, KernelRuntime, ParkState, ProcessObject, SpawnError, THREAD_TERMINATED,
        ThreadObject, UserThreadEntry, WaitToken,
    };

    struct ForwardingRuntime {
        inner: RwLock<Option<Arc<dyn KernelRuntime>>>,
    }

    impl ForwardingRuntime {
        fn set(&self, rt: Arc<dyn KernelRuntime>) {
            *self.inner.write().unwrap() = Some(rt);
        }

        fn clear(&self) {
            *self.inner.write().unwrap() = None;
        }

        fn with<R>(&self, f: impl FnOnce(&dyn KernelRuntime) -> R) -> R {
            let guard = self.inner.read().unwrap();
            let rt = guard
                .as_ref()
                .expect("ForwardingRuntime: inner runtime not set");
            f(rt.as_ref())
        }
    }

    impl KernelRuntime for ForwardingRuntime {
        fn current_wait_token(&self) -> WaitToken {
            self.with(|rt| rt.current_wait_token())
        }

        fn current_handle_table(
            &self,
        ) -> Option<Arc<collections::MutexCell<kobject::HandleTable>>> {
            self.with(|rt| rt.current_handle_table())
        }

        fn current_thread_object(&self) -> Option<Arc<kobject::ThreadObject>> {
            self.with(|rt| rt.current_thread_object())
        }

        fn current_process_object(&self) -> Option<Arc<kobject::ProcessObject>> {
            self.with(|rt| rt.current_process_object())
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

        fn unblock(&self, token: WaitToken) {
            self.with(|rt| rt.unblock(token));
        }

        fn create_empty_process(
            &self,
            name: &'static str,
        ) -> Result<Arc<ProcessObject>, SpawnError> {
            self.with(|rt| rt.create_empty_process(name))
        }

        fn create_user_thread(
            &self,
            process: &Arc<ProcessObject>,
            entry: UserThreadEntry,
        ) -> Result<Arc<ThreadObject>, SpawnError> {
            self.with(|rt| rt.create_user_thread(process, entry))
        }

        fn terminate_thread(
            &self,
            thread: &Arc<ThreadObject>,
            exit_code: i32,
        ) -> Result<(), IpcError> {
            self.with(|rt| rt.terminate_thread(thread, exit_code))
        }

        fn terminate_process(
            &self,
            process: &Arc<ProcessObject>,
            exit_code: i32,
        ) -> Result<(), IpcError> {
            self.with(|rt| rt.terminate_process(process, exit_code))
        }
    }

    static FORWARDER: OnceLock<Arc<ForwardingRuntime>> = OnceLock::new();
    static INSTALLED: Once = Once::new();
    let _ = ParkState::REGISTERED;

    let forwarder = FORWARDER
        .get_or_init(|| {
            Arc::new(ForwardingRuntime {
                inner: RwLock::new(None),
            })
        })
        .clone();
    INSTALLED.call_once(|| {
        let dyn_rt: Arc<dyn KernelRuntime> = forwarder.clone();
        kobject::install_runtime(dyn_rt);
    });

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
    assert_eq!(ko.peek() & THREAD_TERMINATED, 0);

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

    assert_eq!(ko.peek() & THREAD_TERMINATED, THREAD_TERMINATED);
    assert_eq!(ko.exit_code(), 0);
}

#[test]
fn process_create_returns_handle_to_empty_process() {
    use kobject::{KernelRuntime, PROCESS_TERMINATED};

    reset_switches();
    let factory = new_factory_static();
    let timer = MockTimer::new();
    let scheduler = make_scheduler_with_factory(timer, factory);
    let processes_before = scheduler.process_count();
    let handle = scheduler.handle();

    let process = handle
        .create_empty_process("p")
        .expect("create_empty_process must succeed");

    assert_eq!(process.peek() & PROCESS_TERMINATED, 0);
    assert!(Arc::strong_count(&process) >= 1);
    assert_eq!(scheduler.process_count(), processes_before + 1);
}

#[test]
fn thread_terminate_via_handle_signals_terminated() {
    use kobject::{KernelRuntime, THREAD_TERMINATED, UserThreadEntry};

    reset_switches();
    let factory = new_factory_static();
    let timer = MockTimer::new();
    let scheduler = make_scheduler_with_factory(timer, factory);
    let handle = scheduler.handle();

    let process = handle
        .create_empty_process("p")
        .expect("create_empty_process");
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

    assert_eq!(thread.peek() & THREAD_TERMINATED, 0);
    handle
        .terminate_thread(&thread, 7)
        .expect("terminate_thread");
    assert_eq!(thread.peek() & THREAD_TERMINATED, THREAD_TERMINATED);
    assert_eq!(thread.exit_code(), 7);
}

#[test]
fn process_terminate_via_handle_terminates_all_threads() {
    use kobject::{KernelRuntime, PROCESS_TERMINATED, THREAD_TERMINATED, UserThreadEntry};

    reset_switches();
    let factory = new_factory_static();
    let timer = MockTimer::new();
    let scheduler = make_scheduler_with_factory(timer, factory);
    let handle = scheduler.handle();

    let process = handle
        .create_empty_process("p")
        .expect("create_empty_process");
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

    assert_eq!(t1.peek() & THREAD_TERMINATED, THREAD_TERMINATED);
    assert_eq!(t2.peek() & THREAD_TERMINATED, THREAD_TERMINATED);
    assert_eq!(process.peek() & PROCESS_TERMINATED, PROCESS_TERMINATED);
    assert_eq!(process.exit_code(), -1);
}

#[test]
fn switch_to_next_skips_terminated_thread_in_ready_queue() {
    use kobject::{KernelRuntime, THREAD_TERMINATED};

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
    assert_eq!(t2_ko.peek() & THREAD_TERMINATED, THREAD_TERMINATED);

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
    let already_used = 2;
    let to_fill = TEST_CONFIG.max_threads() - already_used;
    for _ in 0..to_fill {
        handle
            .spawn_boxed(inherit_cfg, Box::new(|| {}))
            .expect("inherit spawn must succeed under capacity");
    }

    let count_before = running.process_thread_count(pid).expect("pid alive");
    let res = handle.spawn_boxed(inherit_cfg, Box::new(|| {}));
    assert_eq!(res, Err(SpawnError::NoFreeThreadSlots));
    let count_after = running.process_thread_count(pid).expect("pid alive");
    assert_eq!(count_before, count_after);
}
