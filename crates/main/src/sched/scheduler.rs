#![allow(unsafe_code)]

use alloc::{boxed::Box, sync::Arc, vec::Vec};
use core::marker::PhantomData;

use collections::{LockCell, MutexCell};
use drivers_common::services::scheduler::{
    Priority, SpawnAddressSpace, SpawnConfig, SpawnError, ThreadId,
};
use memory::memory_mapper::{AddressSpaceFactory, AddressSpaceHandle};

use super::{
    address_space::AddressSpace,
    arch::{ArchContext, ArchCpu, CpuId, TimerSource, with_preemption_disabled},
    cpu::Cpu,
    process::{ProcessId, ProcessTable},
    thread::{Thread, ThreadState},
    thread_table::ThreadTable,
    wait_queue::{SleepEntry, SleepQueue},
};
use crate::kobject::HandleTable;

const DEFAULT_TIME_SLICE_TICKS: u32 = 1;
const DEFAULT_QUANTUM_NS: u64 = 10_000_000;

/// Решение scheduler-а о судьбе AS на context switch'е.
#[derive(Clone, Copy, Debug)]
pub(super) enum AddressSpaceTransition {
    /// Process не сменился, AS активен - переключение не требуется.
    Keep,
    /// Process сменился; перед `A::switch` вызвать `switch_address_space(handle)`.
    Switch(Option<AddressSpaceHandle>),
}

/// Действие, которое scheduler-инициатор должен выполнить ПОСЛЕ
/// освобождения внутреннего lock-а.
pub(super) enum ScheduleAction<A: ArchContext> {
    None,
    Switch {
        prev: *mut A,
        next: *const A,
        address_space: AddressSpaceTransition,
    },
}

impl<A: ArchContext> Clone for ScheduleAction<A> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<A: ArchContext> Copy for ScheduleAction<A> {}

/// Type-state маркеры состояния scheduler-а.
pub trait SchedulerStage {}

/// Scheduler создан, но per-CPU состояние ещё не установлено.
pub struct Uninit;

/// Per-CPU состояние установлено.
/// Доступен `spawn` для bootstrap-кода и `handle()` для регистрации сервиса.
pub struct Bootstrapped;

/// Произошёл первый context switch - scheduler управляет потоками.
/// Доступны `yield_now`, `sleep_ns`, `on_tick`, `exit`.
pub struct Running;

impl SchedulerStage for Uninit {}
impl SchedulerStage for Bootstrapped {}
impl SchedulerStage for Running {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SchedulerConfig {
    priority_levels: usize,
    max_threads: usize,
}

impl SchedulerConfig {
    pub const fn new(priority_levels: usize, max_threads: usize) -> Self {
        assert!(priority_levels > 0 && priority_levels <= 32);
        assert!(max_threads > 0);

        Self {
            priority_levels,
            max_threads,
        }
    }

    pub const fn priority_levels(self) -> usize {
        self.priority_levels
    }

    pub const fn max_threads(self) -> usize {
        self.max_threads
    }
}

pub struct Scheduler<A, T, S>
where
    A: ArchContext,
    T: TimerSource,
    S: SchedulerStage,
{
    pub(crate) inner: Arc<MutexCell<SchedulerInner<A, T>>>,
    _stage: PhantomData<S>,
}

pub(crate) struct SchedulerInner<A, T>
where
    A: ArchContext,
    T: TimerSource,
{
    config: SchedulerConfig,
    timer: T,
    threads: ThreadTable<A>,
    processes: ProcessTable,
    cpus: Vec<Option<Box<Cpu>>>,
    sleepers: SleepQueue,
    kernel_address_space: Arc<AddressSpace>,
    address_space_factory: Option<&'static dyn AddressSpaceFactory>,
    quantum_ns: u64,
    time_slice_ticks: u32,
}

/// Возвращает приоритет, используемый для idle-потока в scheduler с указанным
/// количеством уровней приоритета.
pub const fn lowest_priority(priority_levels: usize) -> Priority {
    assert!(priority_levels > 0, "priority_levels must be > 0");
    Priority::new((priority_levels - 1) as u8)
}

impl<A, T> Scheduler<A, T, Uninit>
where
    A: ArchContext,
    T: TimerSource,
{
    pub fn new(timer: T, config: SchedulerConfig) -> Self {
        Self::with_address_space_factory(timer, config, None)
    }

    /// Создаёт scheduler с привязанной фабрикой user-AS. Если фабрика
    /// `None`, scheduler не сможет обработать `SpawnAddressSpace::User`
    /// и вернёт `SpawnError::AddressSpaceCreationFailed` - это режим
    /// host-тестов с MockContext.
    pub fn with_address_space_factory(
        timer: T,
        config: SchedulerConfig,
        address_space_factory: Option<&'static dyn AddressSpaceFactory>,
    ) -> Self {
        Self {
            inner: Arc::new(MutexCell::new(SchedulerInner {
                config,
                timer,
                threads: ThreadTable::new(config.max_threads()),
                processes: ProcessTable::new(config.max_threads()),
                cpus: Vec::new(),
                sleepers: SleepQueue::new(),
                kernel_address_space: AddressSpace::kernel(),
                address_space_factory,
                quantum_ns: DEFAULT_QUANTUM_NS,
                time_slice_ticks: DEFAULT_TIME_SLICE_TICKS,
            })),
            _stage: PhantomData,
        }
    }

    /// Создаёт per-CPU state, idle-поток и устанавливает `TPIDR_EL1`.
    /// Должен вызываться при замаскированных IRQ.
    pub fn bootstrap(self) -> Scheduler<A, T, Bootstrapped> {
        let bootstrapped = Scheduler {
            inner: self.inner,
            _stage: PhantomData::<Bootstrapped>,
        };

        bootstrapped
            .inner
            .with_lock(SchedulerInner::bootstrap_current_cpu);

        bootstrapped
    }
}

impl<A, T> Scheduler<A, T, Bootstrapped>
where
    A: ArchContext,
    T: TimerSource,
{
    /// Возвращает thread-safe handle на scheduler для регистрации в `Capabilities`
    /// и использования в trampoline-замыканиях.
    pub fn handle(&self) -> super::service::SchedulerHandle<A, T> {
        super::service::SchedulerHandle::new(self.inner.clone())
    }

    /// Спавн потока в bootstrap-фазе. Должен вызываться при замаскированных IRQ
    /// до перехода в `Running`-состояние.
    pub fn spawn<F>(&self, cfg: SpawnConfig, entry: F) -> Result<ThreadId, SpawnError>
    where
        F: FnOnce() + Send + 'static,
    {
        let exit_handle: Arc<dyn drivers_common::services::scheduler::SchedulerService> =
            Arc::new(self.handle());
        self.inner
            .with_lock(|inner| inner.spawn(cfg, entry, exit_handle))
    }

    /// Тестовый путь старта. Выполняет первый switch через `A::switch`,
    /// возвращая управление вызывающему. Используется для unit-тестов с
    /// `MockContext`. В реальном boot-сценарии используйте [`Scheduler::start`].
    pub fn run(self) -> Scheduler<A, T, Running> {
        let running = Scheduler {
            inner: self.inner,
            _stage: PhantomData::<Running>,
        };

        let action = with_preemption_disabled::<A::Cpu, _>(|| {
            running.inner.with_lock(|inner| {
                let now_ns = inner.timer.now_ns();
                inner.switch_to_next(now_ns)
            })
        });
        perform_schedule_action::<A>(action);

        running
    }

    /// Канонический boot-путь. Выбирает первый поток, выполняет неосвобождающий
    /// прыжок через `A::start` и не возвращается.
    pub fn start(self) -> ! {
        <A::Cpu as ArchCpu>::disable_preemption();
        let (next_ptr, address_space) = self
            .inner
            .with_lock(SchedulerInner::prepare_first_thread_start);
        // Перед `eret` в новый thread активируем его AS (или kernel-only - idle).
        A::switch_address_space(address_space);
        // SAFETY: `prepare_first_thread_start` под scheduler-lock выбирает первый
        // runnable поток и возвращает указатель на его уже инициализированный
        // архитектурный контекст, который хранится в таблице потоков scheduler-а.
        // Preemption/IRQ здесь всё ещё замаскированы, как того требует `A::start`;
        // они будут разрешены только после входа в поток через trampoline.
        unsafe { A::start(&*next_ptr) }
    }
}

impl<A, T> Scheduler<A, T, Running>
where
    A: ArchContext,
    T: TimerSource,
{
    pub fn current(&self) -> ThreadId {
        self.inner.with_lock(|inner| inner.current())
    }

    pub fn yield_now(&self) {
        let action = with_preemption_disabled::<A::Cpu, _>(|| {
            self.inner.with_lock(|inner| {
                let now_ns = inner.timer.now_ns();
                inner.yield_now(now_ns)
            })
        });
        perform_schedule_action::<A>(action);
    }

    pub fn sleep_ns(&self, ns: u64) {
        let action = with_preemption_disabled::<A::Cpu, _>(|| {
            self.inner.with_lock(|inner| {
                let now_ns = inner.timer.now_ns();
                inner.sleep_current(ns, now_ns)
            })
        });
        perform_schedule_action::<A>(action);
    }

    pub fn on_tick(&self, now_ns: u64) {
        let action = self.inner.with_lock(|inner| inner.on_tick(now_ns));
        perform_schedule_action::<A>(action);
    }

    pub fn handle(&self) -> super::service::SchedulerHandle<A, T> {
        super::service::SchedulerHandle::new(self.inner.clone())
    }
}

impl<A, T> SchedulerInner<A, T>
where
    A: ArchContext,
    T: TimerSource,
{
    pub(crate) fn spawn_boxed(
        &mut self,
        cfg: SpawnConfig,
        entry: Box<dyn FnOnce() + Send + 'static>,
        exit_handle: Arc<dyn drivers_common::services::scheduler::SchedulerService>,
    ) -> Result<ThreadId, SpawnError> {
        // Box<dyn FnOnce()> уже реализует FnOnce(), поэтому передаём напрямую.
        self.spawn(cfg, entry, exit_handle)
    }

    fn bootstrap_current_cpu(&mut self) {
        let cpu_id = <A::Cpu as ArchCpu>::current_id();
        let idle_priority = lowest_priority(self.config.priority_levels());
        let idle_id = self
            .spawn(
                SpawnConfig::new("idle").priority(idle_priority),
                || <A::Cpu as ArchCpu>::idle(),
                Self::idle_exit_handle(),
            )
            .expect("idle thread bootstrap must succeed");

        let idle = self
            .threads
            .get_mut(idle_id)
            .expect("idle thread must exist after bootstrap");
        idle.set_state(ThreadState::Running);
        idle.set_time_slice_left(self.time_slice_ticks);

        let mut cpu = Box::new(Cpu::new(cpu_id, idle_id, self.config.priority_levels()));
        let cpu_ptr = core::ptr::from_mut::<Cpu>(&mut cpu).cast::<()>();

        self.ensure_cpu_slot(cpu_id);
        let slot = self
            .cpus
            .get_mut(cpu_id.as_index())
            .expect("CPU slot must exist after ensure_cpu_slot");
        *slot = Some(cpu);

        // SAFETY: Box<Cpu> хранится в slot до конца жизни scheduler-а; адрес стабилен.
        unsafe { <A::Cpu as ArchCpu>::install_cpu_local(cpu_ptr) };
    }

    /// Заглушка для idle-потока, который никогда не возвращается из FnOnce.
    /// `exit_handle` нужен только из-за общего `spawn`-API.
    fn idle_exit_handle() -> Arc<dyn drivers_common::services::scheduler::SchedulerService> {
        Arc::new(IdleExitStub)
    }

    fn spawn<F>(
        &mut self,
        cfg: SpawnConfig,
        entry: F,
        exit_handle: Arc<dyn drivers_common::services::scheduler::SchedulerService>,
    ) -> Result<ThreadId, SpawnError>
    where
        F: FnOnce() + Send + 'static,
    {
        if cfg.stack_pages == 0 {
            return Err(SpawnError::InvalidStackPages);
        }
        if (cfg.priority.raw() as usize) >= self.config.priority_levels() {
            return Err(SpawnError::InvalidPriority);
        }

        let stack = <A::Stack as super::arch::ThreadStackAllocator>::allocate(cfg.stack_pages)
            .map_err(|_| SpawnError::StackAllocationFailed)?;
        let stack_top = stack.top();

        let entry_box: Box<dyn FnOnce() + Send + 'static> = Box::new(entry);
        let payload = TrampolinePayload {
            entry: entry_box,
            exit_handle,
        };
        let arg = Box::into_raw(Box::new(payload)).cast::<()>();
        let arch = A::init(stack_top, thread_trampoline::<A>, arg);

        let address_space = self.resolve_spawn_address_space(cfg.address_space)?;
        let process_id = self
            .processes
            .insert(cfg.name, address_space)
            .map_err(|_| SpawnError::NoFreeThreadSlots)?;

        let cpu_affinity = self
            .current_cpu()
            .map_or_else(<A::Cpu as ArchCpu>::current_id, Cpu::id);

        let id = self.threads.insert_with(|id| {
            Thread::new(
                id,
                process_id,
                cpu_affinity,
                cfg.priority,
                arch,
                stack,
                cfg.name,
            )
        })?;

        if let Some(cpu) = self.cpu_by_id_mut(cpu_affinity)
            && cpu.idle() != id
        {
            cpu.ready_queue_mut().push(id, cfg.priority);
        }

        Ok(id)
    }

    /// Создаёт `Arc<AddressSpace>` для нового потока согласно `SpawnAddressSpace`.
    /// `Inherit` использует AS текущего процесса (кroме bootstrap-фазы - там
    /// fallback на kernel-AS, как у kernel-thread).
    fn resolve_spawn_address_space(
        &self,
        spec: SpawnAddressSpace,
    ) -> Result<Arc<AddressSpace>, SpawnError> {
        match spec {
            SpawnAddressSpace::Kernel => Ok(self.kernel_address_space.clone()),
            SpawnAddressSpace::Inherit => Ok(self
                .current_address_space()
                .unwrap_or_else(|| self.kernel_address_space.clone())),
            SpawnAddressSpace::User => {
                let factory = self
                    .address_space_factory
                    .ok_or(SpawnError::AddressSpaceCreationFailed)?;
                AddressSpace::new_user(factory).map_err(|_| SpawnError::AddressSpaceCreationFailed)
            }
        }
    }

    fn current_address_space(&self) -> Option<Arc<AddressSpace>> {
        let current_id = self.current_cpu()?.current();
        let pid = self.threads.get(current_id)?.process();
        Some(self.processes.get(pid)?.address_space().clone())
    }

    pub(super) fn on_tick(&mut self, now_ns: u64) -> ScheduleAction<A> {
        self.wake_sleepers(now_ns);

        let Some((current_id, idle_id, highest_ready)) = self.current_cpu().map(|cpu| {
            (
                cpu.current(),
                cpu.idle(),
                cpu.ready_queue().peek_highest_priority(),
            )
        }) else {
            return ScheduleAction::None;
        };

        let should_preempt = {
            let Some(current) = self.threads.get_mut(current_id) else {
                self.schedule_next_deadline(now_ns);
                return ScheduleAction::None;
            };

            if current_id == idle_id {
                highest_ready.is_some()
            } else {
                let remaining = current.time_slice_left().saturating_sub(1);
                current.set_time_slice_left(remaining);
                remaining == 0 || highest_ready.is_some_and(|prio| prio < current.priority())
            }
        };

        if should_preempt {
            self.preempt_current(now_ns)
        } else {
            self.schedule_next_deadline(now_ns);
            ScheduleAction::None
        }
    }

    pub(super) fn yield_now(&mut self, now_ns: u64) -> ScheduleAction<A> {
        if let Some(cpu) = self.current_cpu()
            && cpu.ready_queue().is_empty()
        {
            self.schedule_next_deadline(now_ns);
            return ScheduleAction::None;
        }

        self.preempt_current(now_ns)
    }

    pub(super) fn sleep_current(&mut self, ns: u64, now_ns: u64) -> ScheduleAction<A> {
        if ns == 0 {
            return self.yield_now(now_ns);
        }

        let wakeup_at_ns = now_ns.saturating_add(ns);
        let current_id = self.current();
        if let Some(thread) = self.threads.get_mut(current_id) {
            thread.set_state(ThreadState::Sleeping { wakeup_at_ns });
            thread.set_time_slice_left(self.time_slice_ticks);
            self.sleepers.push(SleepEntry {
                wakeup_at_ns,
                thread_id: current_id,
            });
        }

        self.switch_to_next(now_ns)
    }

    /// Парк current thread в KO-wait: при отсутствии `timeout_ns` -
    /// бессрочно (`Blocked`), при наличии - до заданного абсолютного
    /// дедлайна через тот же `SleepQueue`, что и `sleep_current`. После
    /// возврата `ScheduleAction` выполняется context switch; thread
    /// возобновится в [`Self::unblock_thread`] либо в `wake_sleepers`.
    pub(super) fn block_current(
        &mut self,
        now_ns: u64,
        timeout_ns: Option<u64>,
    ) -> ScheduleAction<A> {
        let current_id = self.current();
        if let Some(thread) = self.threads.get_mut(current_id) {
            thread.set_time_slice_left(self.time_slice_ticks);
            if let Some(timeout) = timeout_ns {
                let wakeup_at_ns = now_ns.saturating_add(timeout);
                thread.set_state(ThreadState::Sleeping { wakeup_at_ns });
                self.sleepers.push(SleepEntry {
                    wakeup_at_ns,
                    thread_id: current_id,
                });
            } else {
                thread.set_state(ThreadState::Blocked);
            }
        }

        self.switch_to_next(now_ns)
    }

    /// Будит ранее заблокированный (через [`Self::block_current`])
    /// поток, переводя его в `Ready` и помещая в очередь готовых.
    /// Idempotent относительно других состояний - повторный вызов
    /// или race с timeout-стороной (где waker'у уже не выпала
    /// возможность поймать `Blocked`/`Sleeping`) не имеет эффекта.
    pub(super) fn unblock_thread(&mut self, id: ThreadId) {
        let Some(thread) = self.threads.get_mut(id) else {
            return;
        };
        match thread.state() {
            ThreadState::Blocked | ThreadState::Sleeping { .. } => {
                let priority = thread.priority();
                let affinity = thread.cpu_affinity();
                thread.set_state(ThreadState::Ready);
                thread.set_time_slice_left(self.time_slice_ticks);
                if let Some(cpu) = self.cpu_by_id_mut(affinity) {
                    cpu.ready_queue_mut().push(id, priority);
                }
            }
            _ => {}
        }
    }

    /// Клонирует `Arc` per-process таблицы handle'ов текущего потока.
    /// Возвращает `None`, если scheduler ещё не bootstrapped или текущий
    /// поток/процесс не зарегистрирован.
    pub(super) fn current_handle_table(&self) -> Option<Arc<MutexCell<HandleTable>>> {
        let current_id = self.current_cpu()?.current();
        let pid = self.threads.get(current_id)?.process();
        self.processes.get(pid).map(|p| p.handle_table().clone())
    }

    pub(super) fn current(&self) -> ThreadId {
        self.current_cpu()
            .expect("scheduler must be bootstrapped before current()")
            .current()
    }

    pub(super) fn now_ns(&self) -> u64 {
        self.timer.now_ns()
    }

    pub(super) fn exit_current(&mut self, now_ns: u64) -> ScheduleAction<A> {
        let current_id = self.current();
        if let Some(thread) = self.threads.get_mut(current_id) {
            thread.set_state(ThreadState::Terminated);
        }

        self.switch_to_next(now_ns)
    }

    fn wake_sleepers(&mut self, now_ns: u64) {
        while self
            .sleepers
            .peek()
            .is_some_and(|entry| entry.wakeup_at_ns <= now_ns)
        {
            let entry = self.sleepers.pop().expect("peek returned entry");
            let Some(thread) = self.threads.get_mut(entry.thread_id) else {
                continue;
            };
            // Сравниваем deadline'ы: если поток уже разбужен по signal-стороне
            // и снова ушёл в Sleeping с другим wakeup_at_ns - старая stale-entry
            // не должна тригернуть преждевременное пробуждение.
            if !matches!(
                thread.state(),
                ThreadState::Sleeping { wakeup_at_ns } if wakeup_at_ns == entry.wakeup_at_ns
            ) {
                continue;
            }
            let priority = thread.priority();
            let affinity = thread.cpu_affinity();
            thread.set_state(ThreadState::Ready);
            thread.set_time_slice_left(self.time_slice_ticks);

            if let Some(cpu) = self.cpu_by_id_mut(affinity) {
                cpu.ready_queue_mut().push(entry.thread_id, priority);
            }
        }
    }

    fn preempt_current(&mut self, now_ns: u64) -> ScheduleAction<A> {
        let current_id = self.current();
        let idle_id = self.current_cpu().expect("cpu").idle();

        if current_id != idle_id {
            let priority = if let Some(current) = self.threads.get_mut(current_id) {
                if matches!(current.state(), ThreadState::Running) {
                    current.set_state(ThreadState::Ready);
                    current.set_time_slice_left(self.time_slice_ticks);
                    Some(current.priority())
                } else {
                    None
                }
            } else {
                None
            };

            if let Some(priority) = priority
                && let Some(cpu) = self.current_cpu_mut()
            {
                cpu.ready_queue_mut().push(current_id, priority);
            }
        }

        self.switch_to_next(now_ns)
    }

    fn switch_to_next(&mut self, now_ns: u64) -> ScheduleAction<A> {
        let Some(cpu) = self.current_cpu_mut() else {
            return ScheduleAction::None;
        };

        let prev_id = cpu.current();
        let idle_id = cpu.idle();
        let next_id = cpu
            .ready_queue_mut()
            .pop_highest()
            .map_or(idle_id, |(id, _)| id);

        cpu.set_current(next_id);

        if let Some(next) = self.threads.get_mut(next_id) {
            next.set_state(ThreadState::Running);
            next.set_time_slice_left(self.time_slice_ticks);
        }

        if prev_id == next_id {
            self.schedule_next_deadline(now_ns);
            return ScheduleAction::None;
        }

        let from_proc = self.threads.get(prev_id).map(Thread::process);
        let to_proc = self.threads.get(next_id).map(Thread::process);
        let address_space = self.address_space_change(from_proc, to_proc);

        let Some((prev, next)) = self.threads.split_pair_mut(prev_id, next_id) else {
            self.schedule_next_deadline(now_ns);
            return ScheduleAction::None;
        };

        assert!(
            prev.stack().check_canary(),
            "stack overflow detected in thread '{}' (id {:?}, base 0x{:x})",
            prev.name(),
            prev.id(),
            prev.stack().base_addr()
        );

        if matches!(prev.state(), ThreadState::Running) {
            prev.set_state(ThreadState::Ready);
        }

        let action = ScheduleAction::Switch {
            prev: core::ptr::from_mut(prev.arch_mut()),
            next: core::ptr::from_ref(next.arch()),
            address_space,
        };
        self.schedule_next_deadline(now_ns);
        action
    }

    /// `Switch(root)` если переход меняет process. `Keep` - процесс тот же
    /// (или next-thread не зарегистрирован, что не должно случаться на
    /// здоровом scheduler-пути).
    fn address_space_change(
        &self,
        from_proc: Option<ProcessId>,
        to_proc: Option<ProcessId>,
    ) -> AddressSpaceTransition {
        let Some(target) = to_proc else {
            return AddressSpaceTransition::Keep;
        };
        if from_proc == Some(target) {
            return AddressSpaceTransition::Keep;
        }
        match self.processes.get(target) {
            Some(proc) => AddressSpaceTransition::Switch(proc.address_space().handle()),
            None => AddressSpaceTransition::Keep,
        }
    }

    fn prepare_first_thread_start(&mut self) -> (*const A, Option<AddressSpaceHandle>) {
        let now_ns = self.timer.now_ns();
        let cpu = self
            .current_cpu_mut()
            .expect("scheduler must be bootstrapped");
        let idle_id = cpu.idle();
        let next_id = cpu
            .ready_queue_mut()
            .pop_highest()
            .map_or(idle_id, |(id, _)| id);

        cpu.set_current(next_id);

        let address_space = self
            .threads
            .get(next_id)
            .map(Thread::process)
            .and_then(|pid| self.processes.get(pid))
            .and_then(|p| p.address_space().handle());

        let next_ptr = {
            let next = self
                .threads
                .get_mut(next_id)
                .expect("first runnable thread must exist");
            next.set_state(ThreadState::Running);
            next.set_time_slice_left(self.time_slice_ticks);
            core::ptr::from_ref(next.arch())
        };
        self.schedule_next_deadline(now_ns);
        (next_ptr, address_space)
    }

    fn schedule_next_deadline(&self, now_ns: u64) {
        let wakeup_deadline = self.sleepers.peek().map(|entry| entry.wakeup_at_ns);
        let quantum_deadline = now_ns.saturating_add(self.quantum_ns);
        let deadline = wakeup_deadline.map_or(quantum_deadline, |wakeup_deadline| {
            wakeup_deadline.min(quantum_deadline)
        });
        self.timer.schedule_next(deadline);
    }

    fn current_cpu(&self) -> Option<&Cpu> {
        let ptr = <A::Cpu as ArchCpu>::cpu_local_ptr();
        if ptr.is_null() {
            // Fallback на индексацию через MPIDR - используется в bootstrap фазе,
            // когда install_cpu_local ещё не выполнен, и в host-тестах с MockCpu.
            let cpu_id = <A::Cpu as ArchCpu>::current_id();
            return self.cpus.get(cpu_id.as_index()).and_then(|s| s.as_deref());
        }
        // SAFETY: ptr был установлен через install_cpu_local; Box<Cpu> жив,
        // указатель валиден всё время жизни scheduler.
        Some(unsafe { &*(ptr.cast::<Cpu>()) })
    }

    fn current_cpu_mut(&mut self) -> Option<&mut Cpu> {
        let ptr = <A::Cpu as ArchCpu>::cpu_local_ptr();
        if ptr.is_null() {
            let cpu_id = <A::Cpu as ArchCpu>::current_id();
            return self
                .cpus
                .get_mut(cpu_id.as_index())
                .and_then(|s| s.as_deref_mut());
        }
        // SAFETY: cm. current_cpu().
        Some(unsafe { &mut *(ptr.cast::<Cpu>()) })
    }

    fn cpu_by_id_mut(&mut self, id: CpuId) -> Option<&mut Cpu> {
        self.cpus
            .get_mut(id.as_index())
            .and_then(|slot| slot.as_deref_mut())
    }

    fn ensure_cpu_slot(&mut self, id: CpuId) {
        let required_len = id.as_index() + 1;
        if self.cpus.len() < required_len {
            self.cpus.resize_with(required_len, || None);
        }
    }
}

/// Аргумент trampoline, передаваемый в первый запуск потока.
struct TrampolinePayload {
    entry: Box<dyn FnOnce() + Send + 'static>,
    exit_handle: Arc<dyn drivers_common::services::scheduler::SchedulerService>,
}

pub(super) fn perform_schedule_action<A: ArchContext>(action: ScheduleAction<A>) {
    let ScheduleAction::Switch {
        prev,
        next,
        address_space,
    } = action
    else {
        return;
    };
    // Process-switch: активируем AS ДО context_switch, чтобы возобновляемый
    // user-thread проснулся уже в своём адресном пространстве.
    if let AddressSpaceTransition::Switch(handle) = address_space {
        A::switch_address_space(handle);
    }
    // SAFETY: raw pointers подготовлены под scheduler-lock-ом и валидны до завершения switch.
    unsafe { A::switch(&mut *prev, &*next) };
}

unsafe extern "C" fn thread_trampoline<A: ArchContext>(arg: *mut ()) -> ! {
    // SAFETY: `arg` создан из `Box::into_raw(Box::new(TrampolinePayload { .. }))` в `spawn`.
    // Реверс: `Box::from_raw` возвращает владение `Box`, после чего `*payload` распаковывает поля.
    let payload: Box<TrampolinePayload> = unsafe { Box::from_raw(arg.cast()) };
    let TrampolinePayload { entry, exit_handle } = *payload;
    <A::Cpu as ArchCpu>::enable_preemption();
    entry();
    exit_handle.exit();
}

/// Stub-сервис, передаваемый идле-потоку. Все методы паникуют - вызов невозможен,
/// так как `idle()` никогда не возвращается, а `spawn` от idle-потока недопустим.
struct IdleExitStub;

impl drivers_common::services::scheduler::SchedulerService for IdleExitStub {
    fn spawn_boxed(
        &self,
        _cfg: SpawnConfig,
        _entry: Box<dyn FnOnce() + Send + 'static>,
    ) -> Result<ThreadId, SpawnError> {
        unreachable!("idle thread must never call spawn_boxed")
    }

    fn yield_now(&self) {
        unreachable!("idle thread must never call yield_now")
    }

    fn sleep_ns(&self, _ns: u64) {
        unreachable!("idle thread must never call sleep_ns")
    }

    fn current(&self) -> ThreadId {
        unreachable!("idle thread must never call current")
    }

    fn exit(&self) -> ! {
        unreachable!("idle thread must never call exit")
    }
}
