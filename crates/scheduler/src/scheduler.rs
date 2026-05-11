#![allow(unsafe_code)]

use alloc::{boxed::Box, sync::Arc, vec::Vec};
use core::marker::PhantomData;

use collections::{LockCell, MutexCell};
use kobject::{HandleTable, ProcessObject, ThreadObject};
use memory::{
    memory_mapper::{AddressSpaceFactory, AddressSpaceHandle},
    user_vm_allocator::UserVmAllocator,
    virtual_address::{PageAlignedVirtualAddress, VirtualAddress},
};

use super::{
    address_space::AddressSpace,
    arch::{ArchContext, ArchCpu, CpuId, TimerSource, with_preemption_disabled},
    cpu::Cpu,
    process::{Process, ProcessTable},
    thread::{Thread, ThreadState},
    thread_table::ThreadTable,
    wait_queue::{SleepEntry, SleepQueue},
};
use crate::{
    PreparedUserProcess, PreparedUserProcessError, Priority, ProcessId, SpawnAddressSpace,
    SpawnConfig, SpawnError, ThreadId, UserBootstrapArg, UserProcessLaunchInfo,
};

const FRAME_SIZE: usize = 4096;

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

#[derive(Default)]
#[must_use = "DeferredSignals содержит отложенные signal_terminated; вызови .emit() вне scheduler-lock'а"]
pub(super) struct DeferredSignals {
    threads: Vec<(Arc<ThreadObject>, i32)>,
    processes: Vec<(Arc<ProcessObject>, i32)>,
}

impl DeferredSignals {
    fn push_thread(&mut self, thread: Arc<ThreadObject>, exit_code: i32) {
        self.threads.push((thread, exit_code));
    }

    fn push_process(&mut self, process: Arc<ProcessObject>, exit_code: i32) {
        self.processes.push((process, exit_code));
    }

    pub(super) fn emit(self) {
        for (thread, exit_code) in self.threads {
            thread.signal_terminated(exit_code);
        }
        for (process, exit_code) in self.processes {
            process.signal_terminated(exit_code);
        }
    }
}

// Ручные impls вместо `#[derive]`: derive подкинул бы `A: Copy`-bound,
// которого `ArchContext` не требует. Поля - сырые указатели и `Copy`-enum,
// `A: Copy` не нужен.
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
    address_space_factory: Option<&'static (dyn AddressSpaceFactory + Send + Sync)>,
    /// Процессы, у которых счётчик thread'ов достиг 0. Удаляются на следующем
    /// `switch_to_next`, когда dying thread уже не current ни на одном CPU.
    pending_process_removals: Vec<ProcessId>,
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
    /// и вернёт `SpawnError::AddressSpaceCreationFailed` - этот режим
    /// подходит для встраивания scheduler-а без поддержки user-AS.
    pub fn with_address_space_factory(
        timer: T,
        config: SchedulerConfig,
        address_space_factory: Option<&'static (dyn AddressSpaceFactory + Send + Sync)>,
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
                pending_process_removals: Vec::new(),
                quantum_ns: DEFAULT_QUANTUM_NS,
                time_slice_ticks: DEFAULT_TIME_SLICE_TICKS,
            })),
            _stage: PhantomData,
        }
    }

    /// Создаёт per-CPU state, idle-поток и устанавливает CPU-local указатель.
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
    /// Возвращает thread-safe handle на scheduler для регистрации в bootstrap
    /// services и использования в trampoline-замыканиях.
    pub fn handle(&self) -> super::service::SchedulerHandle<A, T> {
        super::service::SchedulerHandle::new(self.inner.clone())
    }

    /// Спавн потока в bootstrap-фазе. Должен вызываться при замаскированных IRQ
    /// до перехода в `Running`-состояние.
    pub fn spawn<F>(&self, cfg: SpawnConfig, entry: F) -> Result<ThreadId, SpawnError>
    where
        F: FnOnce() + Send + 'static,
    {
        self.inner.with_lock(|inner| inner.spawn(cfg, entry))
    }

    /// Регистрирует уже подготовленный user-процесс.
    pub fn spawn_prepared_user_process(
        &self,
        prepared: PreparedUserProcess,
    ) -> Result<UserProcessLaunchInfo, PreparedUserProcessError> {
        self.inner
            .with_lock(|inner| inner.spawn_prepared_user_process(prepared))
    }

    /// Альтернативный путь старта: выполняет первый switch через `A::switch`,
    /// возвращая управление вызывающему. В реальном boot-сценарии используйте
    /// [`Scheduler::start`].
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
        // Перед первым входом в thread активируем его AS (или kernel-only - idle).
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

    /// Завершает текущий thread и переключается на следующий, возвращая
    /// управление вызывающему. Production-handler `SchedulerHandle::exit -> !`
    /// оборачивает этот метод, добавляя контракт `noreturn`.
    pub fn exit_current(&self) {
        self.exit_current_with_code(0);
    }

    /// Тестовый помощник: вариант [`Self::exit_current`] с явным `exit_code`.
    #[doc(hidden)]
    pub fn exit_current_with_code(&self, exit_code: i32) {
        // Bare disable: preempt должен оставаться выключенным до
        // `perform_schedule_action`, иначе tick может уйти с Terminated
        // потока до `finish_exit_current`. Парный enable не нужен.
        <A::Cpu as ArchCpu>::disable_preemption();
        let signals = self
            .inner
            .with_lock(|inner| inner.begin_exit_current(exit_code));
        signals.emit();
        let action = self.inner.with_lock(|inner| {
            let now_ns = inner.timer.now_ns();
            inner.finish_exit_current(now_ns)
        });
        perform_schedule_action::<A>(action);
    }

    pub fn handle(&self) -> super::service::SchedulerHandle<A, T> {
        super::service::SchedulerHandle::new(self.inner.clone())
    }

    pub fn spawn_prepared_user_process(
        &self,
        prepared: PreparedUserProcess,
    ) -> Result<UserProcessLaunchInfo, PreparedUserProcessError> {
        self.inner
            .with_lock(|inner| inner.spawn_prepared_user_process(prepared))
    }
}

impl<A, T, S> Scheduler<A, T, S>
where
    A: ArchContext,
    T: TimerSource,
    S: SchedulerStage,
{
    pub fn address_space_factory(
        &self,
    ) -> Option<&'static (dyn AddressSpaceFactory + Send + Sync)> {
        self.inner.with_lock(|inner| inner.address_space_factory)
    }

    /// Количество живых процессов в `ProcessTable`. Используется для
    /// диагностики и проверок lifecycle.
    pub fn process_count(&self) -> usize {
        self.inner.with_lock(|inner| inner.processes.live_count())
    }

    /// Количество живых thread'ов процесса `pid`. `None`, если процесс
    /// отсутствует.
    pub fn process_thread_count(&self, pid: ProcessId) -> Option<usize> {
        self.inner
            .with_lock(|inner| inner.processes.get(pid).map(Process::thread_count))
    }

    /// Количество user-VM-регионов, выделенных у процесса `pid`. `None`,
    /// если процесс не найден или у него нет user_vm-аллокатора.
    pub fn process_user_vm_region_count(&self, pid: ProcessId) -> Option<usize> {
        use collections::LockCell;
        self.inner.with_lock(|inner| {
            let process = inner.processes.get(pid)?;
            let vm = process.user_vm()?.clone();
            Some(vm.with_lock(|alloc| alloc.live_count()))
        })
    }

    /// `Arc<ThreadObject>` потока `id`. `None`, если поток отсутствует.
    pub fn thread_object_for(&self, id: ThreadId) -> Option<Arc<ThreadObject>> {
        self.inner
            .with_lock(|inner| inner.threads.get(id).map(|t| t.thread_object().clone()))
    }

    /// `Arc<ProcessObject>` процесса `pid`. `None`, если процесс отсутствует.
    pub fn process_object_for(&self, pid: ProcessId) -> Option<Arc<ProcessObject>> {
        self.inner
            .with_lock(|inner| inner.processes.get(pid).map(|p| p.process_object().clone()))
    }

    /// `ProcessId` процесса, к которому привязан thread `id`. `None`, если
    /// поток отсутствует.
    pub fn thread_process_id(&self, id: ThreadId) -> Option<ProcessId> {
        self.inner
            .with_lock(|inner| inner.threads.get(id).map(Thread::process))
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
    ) -> Result<ThreadId, SpawnError> {
        if cfg.stack_pages == 0 {
            return Err(SpawnError::InvalidStackPages);
        }
        if (cfg.priority.raw() as usize) >= self.config.priority_levels() {
            return Err(SpawnError::InvalidPriority);
        }

        let stack = <A::Stack as super::arch::ThreadStackAllocator>::allocate(cfg.stack_pages)
            .map_err(|_| SpawnError::StackAllocationFailed)?;
        let stack_top = stack.top();

        let payload = TrampolinePayload { entry };
        let arg = Box::into_raw(Box::new(payload)).cast::<()>();
        let arch = A::init(stack_top, thread_trampoline::<A>, arg);

        let address_space = self.resolve_spawn_address_space(cfg.address_space)?;
        let (process_id, is_new_process) =
            self.intern_process_for_spawn(cfg.name, address_space, cfg.address_space)?;

        let cpu_affinity = self
            .current_cpu()
            .map_or_else(<A::Cpu as ArchCpu>::current_id, Cpu::id);

        let id = match self.threads.insert_with(|id| {
            Thread::new(
                id,
                process_id,
                cpu_affinity,
                cfg.priority,
                arch,
                stack,
                cfg.name,
            )
        }) {
            Ok(id) => id,
            Err(e) => {
                self.rollback_intern(process_id, is_new_process);
                return Err(e);
            }
        };

        if let Some(cpu) = self.cpu_by_id_mut(cpu_affinity)
            && cpu.idle() != id
        {
            cpu.ready_queue_mut().push(id, cfg.priority);
        }

        Ok(id)
    }

    fn bootstrap_current_cpu(&mut self) {
        let cpu_id = <A::Cpu as ArchCpu>::current_id();
        let idle_priority = lowest_priority(self.config.priority_levels());
        let idle_id = self
            .spawn(SpawnConfig::new("idle").priority(idle_priority), || {
                <A::Cpu as ArchCpu>::idle()
            })
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

    fn spawn<F>(&mut self, cfg: SpawnConfig, entry: F) -> Result<ThreadId, SpawnError>
    where
        F: FnOnce() + Send + 'static,
    {
        self.spawn_boxed(cfg, Box::new(entry))
    }

    fn validate_prepared_user_process(
        &self,
        prepared: &PreparedUserProcess,
    ) -> Result<(), PreparedUserProcessError> {
        if prepared.kernel_stack_pages == 0 {
            return Err(PreparedUserProcessError::Spawn(
                SpawnError::InvalidStackPages,
            ));
        }
        if (prepared.priority.raw() as usize) >= self.config.priority_levels() {
            return Err(PreparedUserProcessError::Spawn(SpawnError::InvalidPriority));
        }
        if let Some(index) = prepared.launch.bootstrap_handle_index
            && index >= prepared.launch.initial_handles.len()
        {
            return Err(PreparedUserProcessError::InvalidBootstrapHandle);
        }
        if prepared.launch.initial_handles.len() > HandleTable::DEFAULT_CAPACITY as usize {
            return Err(PreparedUserProcessError::TooManyInitialHandles);
        }
        Ok(())
    }

    pub(crate) fn spawn_prepared_user_process(
        &mut self,
        prepared: PreparedUserProcess,
    ) -> Result<UserProcessLaunchInfo, PreparedUserProcessError> {
        self.validate_prepared_user_process(&prepared)?;
        let launch = prepared.launch;

        let stack =
            <A::Stack as super::arch::ThreadStackAllocator>::allocate(prepared.kernel_stack_pages)
                .map_err(|_| PreparedUserProcessError::Spawn(SpawnError::StackAllocationFailed))?;
        let stack_top = stack.top();

        let mut initial_handle_ids = Vec::new();
        let bootstrap_handle_index = launch.bootstrap_handle_index;
        let bootstrap_arg = launch.bootstrap_arg;
        let initial_handles = launch.initial_handles;
        let mut initial_handle_insert_failed = false;

        let user_vm = prepared.user_vm;
        let address_space_for_process = prepared.address_space.clone();
        let process_id = self
            .processes
            .insert_with(|id| {
                let mut process = Process::new(id, prepared.name, address_space_for_process);
                if let Some(vm) = user_vm {
                    process = process.with_user_vm(vm);
                }
                process.handle_table().with_lock(|tbl| {
                    for handle in initial_handles {
                        if let Ok(handle_id) = tbl.insert(handle) {
                            initial_handle_ids.push(handle_id);
                        } else {
                            initial_handle_insert_failed = true;
                            break;
                        }
                    }
                });
                process
            })
            .map_err(|_| PreparedUserProcessError::Spawn(SpawnError::NoFreeThreadSlots))?;

        if initial_handle_insert_failed {
            self.processes.remove(process_id);
            return Err(PreparedUserProcessError::TooManyInitialHandles);
        }

        let bootstrap_arg = match bootstrap_handle_index {
            Some(index) => {
                let raw = initial_handle_ids[index].raw().get();
                UserBootstrapArg(u64::from(raw))
            }
            None => bootstrap_arg,
        };

        let arch = A::init_user(crate::UserEntry {
            kernel_stack_top: stack_top,
            user_pc: prepared.user_pc,
            user_sp: prepared.user_sp,
            arg: bootstrap_arg,
        });

        let cpu_affinity = self
            .current_cpu()
            .map_or_else(<A::Cpu as ArchCpu>::current_id, Cpu::id);

        let thread_id = match self.threads.insert_with(|id| {
            Thread::new(
                id,
                process_id,
                cpu_affinity,
                prepared.priority,
                arch,
                stack,
                prepared.name,
            )
        }) {
            Ok(id) => id,
            Err(e) => {
                // Откатываем процесс: иначе `thread_count=1` остаётся без
                // живого thread'а, decrement никогда не вызовется и Drop
                // AS не освободит фреймы. `remove` дропает `Arc<AddressSpace>` -
                // если это была единственная ссылка (а это так на этапе
                // создания), mapper в Drop возвращает все фреймы.
                self.processes.remove(process_id);
                return Err(PreparedUserProcessError::Spawn(e));
            }
        };

        if let Some(cpu) = self.cpu_by_id_mut(cpu_affinity)
            && cpu.idle() != thread_id
        {
            cpu.ready_queue_mut().push(thread_id, prepared.priority);
        }

        let process_object = self
            .processes
            .get(process_id)
            .expect("process must exist after insert")
            .process_object()
            .clone();
        let thread_object = self
            .threads
            .get(thread_id)
            .expect("thread must exist after insert")
            .thread_object()
            .clone();

        Ok(UserProcessLaunchInfo {
            process_id,
            thread_id,
            initial_handle_ids,
            process_object,
            thread_object,
        })
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

    /// Регистрирует новый thread в правильном `Process`:
    /// - `Inherit` - наследует от текущего процесса (инкремент thread_count).
    ///   Если current отсутствует (bootstrap), fallback на новый Process,
    ///   привязанный к kernel-AS - поведение как у `Kernel`.
    /// - `Kernel` / `User` - создаёт новый Process в `ProcessTable`
    ///   (счётчик потоков = 1 от `Process::new`).
    ///
    /// Возвращает пару `(pid, is_new)`: `is_new == true`, если был создан
    /// новый `Process`; `false` - если инкрементирован существующий.
    /// Используется вызывающим для отката на ошибке последующей регистрации
    /// потока.
    fn intern_process_for_spawn(
        &mut self,
        name: &'static str,
        address_space: Arc<AddressSpace>,
        spec: SpawnAddressSpace,
    ) -> Result<(ProcessId, bool), SpawnError> {
        if let SpawnAddressSpace::Inherit = spec
            && let Some(current_pid) = self.current_process_id()
            && let Some(existing) = self.processes.get(current_pid)
        {
            debug_assert!(
                Arc::ptr_eq(existing.address_space(), &address_space),
                "Inherit AS must match current_address_space (race under scheduler-lock is impossible)"
            );
            existing.increment_thread_count();
            return Ok((current_pid, false));
        }
        let pid = self
            .processes
            .insert(name, address_space)
            .map_err(|_| SpawnError::NoFreeThreadSlots)?;
        Ok((pid, true))
    }

    /// Откатывает `intern_process_for_spawn`, когда последующая регистрация
    /// потока в `ThreadTable` провалилась. Для нового процесса - удаляет
    /// его из таблицы (Drop `Arc<AddressSpace>` освободит фреймы); для
    /// унаследованного - снимает паразитный инкремент `thread_count`.
    fn rollback_intern(&mut self, pid: ProcessId, is_new: bool) {
        if is_new {
            self.processes.remove(pid);
        } else if let Some(process) = self.processes.get(pid) {
            let was_last = process.decrement_thread_count();
            debug_assert!(
                !was_last,
                "rollback decrement reached zero on Inherit path; parent thread should still hold a count"
            );
        }
    }

    fn current_process_id(&self) -> Option<ProcessId> {
        let cpu = self.current_cpu()?;
        self.threads.get(cpu.current()).map(Thread::process)
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

    /// Клонирует `Arc<ThreadObject>` текущего потока. `None`, если
    /// scheduler ещё не bootstrapped и current-thread не определён.
    pub(super) fn current_thread_object(&self) -> Option<Arc<ThreadObject>> {
        let current_id = self.current_cpu()?.current();
        self.threads
            .get(current_id)
            .map(|t| t.thread_object().clone())
    }

    /// Клонирует `Arc<ProcessObject>` процесса, к которому привязан
    /// текущий поток. `None`, если scheduler ещё не bootstrapped или
    /// процесс не зарегистрирован.
    pub(super) fn current_process_object(&self) -> Option<Arc<ProcessObject>> {
        let current_id = self.current_cpu()?.current();
        let pid = self.threads.get(current_id)?.process();
        self.processes.get(pid).map(|p| p.process_object().clone())
    }

    /// Снимок (Arc<AddressSpace>, Arc<MutexCell<UserVmAllocator>>) текущего
    /// процесса для syscall-handler-ов user-памяти. `None`, если процесс -
    /// kernel-only либо у него нет user_vm-аллокатора.
    pub(super) fn current_user_vm_pair(
        &self,
    ) -> Option<(Arc<AddressSpace>, Arc<MutexCell<UserVmAllocator>>)> {
        let current_id = self.current_cpu()?.current();
        let pid = self.threads.get(current_id)?.process();
        let process = self.processes.get(pid)?;
        let vm = process.user_vm()?.clone();
        Some((process.address_space().clone(), vm))
    }

    pub(super) fn current(&self) -> ThreadId {
        self.current_cpu()
            .expect("scheduler must be bootstrapped before current()")
            .current()
    }

    pub(super) fn now_ns(&self) -> u64 {
        self.timer.now_ns()
    }

    /// Завершает текущий thread с заданным `exit_code` и переключается на
    /// следующий runnable.
    ///
    /// Контракт ровно одного вызова на дочерний `ThreadObject`/`ProcessObject`
    /// держится scheduler-локом и инвариантом `thread_count > 0`: метод
    /// вызывается под `MutexCell<SchedulerInner>` и до `switch_to_next`,
    /// поэтому конкурирующий exit того же thread/process невозможен.
    pub(super) fn begin_exit_current(&mut self, exit_code: i32) -> DeferredSignals {
        let mut signals = DeferredSignals::default();
        let current_id = self.current();
        let (exiting_pid, thread_ko) = if let Some(thread) = self.threads.get_mut(current_id) {
            thread.set_state(ThreadState::Terminated);
            (Some(thread.process()), Some(thread.thread_object().clone()))
        } else {
            (None, None)
        };

        if let Some(ko) = thread_ko {
            signals.push_thread(ko, exit_code);
        }

        // Декремент thread_count процесса. На нуле поднимаем
        // PROCESS_TERMINATED ДО `cleanup_pending_process_removals` (вызов из
        // `switch_to_next`) и помечаем процесс к удалению - `Arc<ProcessObject>`
        // переживёт запись в `ProcessTable` через держателей handle'ов.
        if let Some(pid) = exiting_pid
            && let Some(process) = self.processes.get(pid)
            && process.decrement_thread_count()
        {
            signals.push_process(process.process_object().clone(), exit_code);
            self.pending_process_removals.push(pid);
        }

        signals
    }

    pub(super) fn finish_exit_current(&mut self, now_ns: u64) -> ScheduleAction<A> {
        self.switch_to_next(now_ns)
    }

    /// Создаёт пустой user-процесс с собственным AS, без потоков и без
    /// user_vm-аллокатора. Возвращает `Arc<ProcessObject>` нового процесса.
    pub(crate) fn create_empty_process(
        &mut self,
        name: &str,
    ) -> Result<Arc<ProcessObject>, SpawnError> {
        if name.is_empty() {
            return Err(SpawnError::InvalidName);
        }
        let factory = self
            .address_space_factory
            .ok_or(SpawnError::AddressSpaceCreationFailed)?;
        let address_space =
            AddressSpace::new_user(factory).map_err(|_| SpawnError::AddressSpaceCreationFailed)?;
        let pid = self
            .processes
            .insert_with(|id| Process::empty(id, name, address_space))
            .map_err(|_| SpawnError::NoFreeThreadSlots)?;
        let process = self
            .processes
            .get(pid)
            .expect("process must exist after insert");
        Ok(process.process_object().clone())
    }

    /// Создаёт user-поток в указанном процессе и помещает его в ready-queue.
    pub(crate) fn create_user_thread(
        &mut self,
        process_ko: &Arc<ProcessObject>,
        entry: kobject::UserThreadEntry,
    ) -> Result<Arc<ThreadObject>, SpawnError> {
        let (thread_id, thread_ko) = self.prepare_user_thread(process_ko, entry)?;
        self.enqueue_user_thread_ready(thread_id);
        Ok(thread_ko)
    }

    /// Создаёт user-поток без enqueue в ready-queue; `enqueue` или
    /// cleanup через [`Self::drop_prepared_thread`] -- забота caller'а.
    pub(crate) fn prepare_user_thread(
        &mut self,
        process_ko: &Arc<ProcessObject>,
        entry: kobject::UserThreadEntry,
    ) -> Result<(ThreadId, Arc<ThreadObject>), SpawnError> {
        if (entry.priority as usize) >= self.config.priority_levels() {
            return Err(SpawnError::InvalidPriority);
        }

        let priority = Priority::new(entry.priority);
        let process_id = self
            .processes
            .iter()
            .find(|p| Arc::ptr_eq(p.process_object(), process_ko))
            .map(Process::id)
            .ok_or(SpawnError::NoFreeThreadSlots)?;
        // Защита от обхода ProcessStart через прямой syscall ThreadCreate:
        // без user_vm первый же fetch улетел бы в page fault.
        if !self
            .processes
            .get(process_id)
            .is_some_and(Process::is_image_loaded)
        {
            return Err(SpawnError::ImageNotLoaded);
        }

        let stack = <A::Stack as super::arch::ThreadStackAllocator>::allocate(
            crate::SpawnConfig::DEFAULT_STACK_PAGES,
        )
        .map_err(|_| SpawnError::StackAllocationFailed)?;
        let stack_top = stack.top();

        let arch = A::init_user(crate::UserEntry {
            kernel_stack_top: stack_top,
            user_pc: VirtualAddress::new(entry.entry_pc as usize),
            user_sp: VirtualAddress::new(entry.user_sp as usize),
            arg: UserBootstrapArg(entry.arg),
        });

        let cpu_affinity = self
            .current_cpu()
            .map_or_else(<A::Cpu as ArchCpu>::current_id, Cpu::id);

        // Инкрементируем счётчик ДО регистрации потока: на ошибке
        // регистрации делаем компенсирующий decrement (без подъёма
        // PROCESS_TERMINATED, т.к. поток так и не был добавлен).
        let process = self
            .processes
            .get(process_id)
            .expect("process must exist after lookup");
        process.increment_thread_count();

        let thread_id = match self.threads.insert_with(|id| {
            Thread::new(
                id,
                process_id,
                cpu_affinity,
                priority,
                arch,
                stack,
                "<user>",
            )
        }) {
            Ok(id) => id,
            Err(e) => {
                let _ = self
                    .processes
                    .get(process_id)
                    .expect("process still present")
                    .decrement_thread_count();
                return Err(e);
            }
        };

        let thread_ko = self
            .threads
            .get(thread_id)
            .expect("thread must exist after insert")
            .thread_object()
            .clone();

        Ok((thread_id, thread_ko))
    }

    /// Пушит ранее prepared user-thread в ready-queue его cpu-affinity.
    pub(crate) fn enqueue_user_thread_ready(&mut self, thread_id: ThreadId) {
        let Some(thread) = self.threads.get(thread_id) else {
            return;
        };
        let priority = thread.priority();
        let cpu_affinity = thread.cpu_affinity();

        if let Some(cpu) = self.cpu_by_id_mut(cpu_affinity)
            && cpu.idle() != thread_id
        {
            cpu.ready_queue_mut().push(thread_id, priority);
        }
    }

    /// Откатывает prepared user-thread, не доехавший до enqueue: удаляет
    /// из `ThreadTable` и декрементирует thread_count процесса.
    pub(crate) fn drop_prepared_thread(&mut self, thread_id: ThreadId) {
        let pid = self.threads.get(thread_id).map(Thread::process);
        if self.threads.remove(thread_id).is_none() {
            return;
        }
        if let Some(pid) = pid
            && let Some(process) = self.processes.get(pid)
        {
            let _ = process.decrement_thread_count();
        }
    }

    /// Устанавливает регионы образа в child AS, маппит user-стек и
    /// прикрепляет per-process `UserVmAllocator`.
    pub(crate) fn load_user_image_into(
        &mut self,
        process_ko: &Arc<ProcessObject>,
        install: &kobject::UserImageInstall,
    ) -> Result<(), kobject::LoadImageError> {
        use kobject::LoadImageError;

        let process = self
            .process_by_object_mut(process_ko)
            .ok_or(LoadImageError::ProcessNotFound)?;
        if process.is_image_loaded()
            || process.thread_count() != 0
            || !process.handle_table_is_empty()
        {
            return Err(LoadImageError::WrongState);
        }

        let mapper_arc = process
            .address_space()
            .mapper_arc()
            .ok_or(LoadImageError::NoUserAddressSpace)?;
        let mapper: &(dyn memory::memory_mapper::MemoryMapper + Send + Sync) = &*mapper_arc;

        // Overflow проверяем до маппинга: set_user_vm идёт последним,
        // и оставлять процесс в полузагруженном состоянии нельзя.
        let user_vm_end_usize = install
            .user_vm_base
            .as_usize()
            .checked_add(install.user_vm_size)
            .ok_or(LoadImageError::UserVmRangeOverflow)?;

        // Сегменты устанавливаются последовательно; при ошибке откатываем
        // уже установленные через `unmap` (фреймы регионов остаются за
        // `MemoryRegion`, освобождаются их `Drop`).
        let mut installed: Vec<(PageAlignedVirtualAddress, usize)> =
            Vec::with_capacity(install.segments.len());
        for seg in &install.segments {
            if let Err(e) = seg.region.install(mapper, seg.va_base, seg.flags) {
                for (va, size) in installed.iter().rev() {
                    let _ = mapper.unmap(*va, *size);
                }
                return Err(LoadImageError::MappingFailed(e));
            }
            installed.push((seg.va_base, seg.mapped_size));
        }

        let stack_base_usize = install
            .user_stack_top
            .as_usize()
            .checked_sub(install.user_stack_size)
            .ok_or(LoadImageError::MappingFailed(
                memory::memory_mapper::MemoryMappingError::VirtualMappingError,
            ))?;
        let stack_base = PageAlignedVirtualAddress::from_usize(stack_base_usize).ok_or(
            LoadImageError::MappingFailed(
                memory::memory_mapper::MemoryMappingError::VirtualMappingError,
            ),
        )?;
        let stack_pages = install.user_stack_size / FRAME_SIZE;
        if let Err(e) = mapper.map(stack_base, stack_pages, &[], memory::MemFlags::user_rw()) {
            for (va, size) in installed.iter().rev() {
                let _ = mapper.unmap(*va, *size);
            }
            return Err(LoadImageError::MappingFailed(e));
        }

        let vm = UserVmAllocator::new(install.user_vm_base, VirtualAddress::new(user_vm_end_usize));
        process
            .set_user_vm(vm)
            .expect("precondition is_image_loaded()==false guarantees fresh user_vm");
        let segments: Vec<Arc<memory::MemoryRegion>> =
            install.segments.iter().map(|s| s.region.clone()).collect();
        process.set_image_segments(segments);
        Ok(())
    }

    /// Стартует первый user-поток. Drain handles делается ПОСЛЕ успешного
    /// `prepare_user_thread`, поэтому любая ошибка до drain оставляет
    /// loader-table с исходными `HandleId`'ами.
    pub(crate) fn start_user_process(
        &mut self,
        process_ko: &Arc<ProcessObject>,
        spec: kobject::UserStartSpec,
    ) -> Result<Arc<ThreadObject>, kobject::StartProcessError> {
        use kobject::StartProcessError;

        let kobject::UserStartSpec {
            entry,
            loader_handle_table,
            handle_ids,
        } = spec;

        {
            let Some(process) = self.process_by_object_mut(process_ko) else {
                return Err(StartProcessError::ProcessNotFound);
            };
            if !process.is_image_loaded()
                || process.thread_count() != 0
                || !process.handle_table_is_empty()
            {
                return Err(StartProcessError::WrongState);
            }
        }

        // Validate handle_ids под loader-lock'ом до drain: existence +
        // TRANSFER + no dupes.
        if let Err(e) = loader_handle_table.with_lock(|tbl| {
            for (i, id) in handle_ids.iter().enumerate() {
                if handle_ids[..i].iter().any(|prev| prev == id) {
                    return Err(kobject::IpcError::BadHandle);
                }
                tbl.get(*id, kobject::Rights::TRANSFER)?;
            }
            Ok(())
        }) {
            return Err(StartProcessError::HandleValidationFailed(e));
        }

        let (thread_id, thread_ko) = match self.prepare_user_thread(process_ko, entry) {
            Ok(t) => t,
            Err(source) => return Err(StartProcessError::SpawnFailed(source.into())),
        };

        // Validate и drain в разных with_lock-окнах; на гонке (другой
        // syscall закрыл handle между шагами) откатываем prepared thread.
        let drained = match loader_handle_table
            .with_lock(|tbl| tbl.try_drain_for_transfer(&handle_ids, kobject::Rights::TRANSFER))
        {
            Ok(d) => d,
            Err(e) => {
                self.drop_prepared_thread(thread_id);
                return Err(StartProcessError::HandleValidationFailed(e));
            }
        };

        // Insert не фейлит: child-table пуста и MAX_BOOTSTRAP_HANDLES
        // много меньше DEFAULT_CAPACITY.
        let child_table = self
            .process_by_object_mut(process_ko)
            .expect("process still present")
            .handle_table()
            .clone();
        child_table.with_lock(|tbl| {
            for h in drained {
                tbl.insert(h)
                    .expect("child handle-table has DEFAULT_CAPACITY slots free");
            }
        });

        self.enqueue_user_thread_ready(thread_id);

        Ok(thread_ko)
    }

    fn process_by_object_mut(&mut self, ko: &Arc<ProcessObject>) -> Option<&mut Process> {
        self.processes
            .iter_mut_internal()
            .find(|p| Arc::ptr_eq(p.process_object(), ko))
    }

    /// Идемпотентно завершает поток через handle: поднимает
    /// `THREAD_TERMINATED`, декрементирует thread_count, на нуле -
    /// `PROCESS_TERMINATED`. Context switch не делает; для завершения
    /// собственного потока должен использоваться [`Self::exit_current`].
    fn collect_thread_termination(
        &mut self,
        thread_ko: &Arc<ThreadObject>,
        exit_code: i32,
        signals: &mut DeferredSignals,
    ) {
        let lookup = self
            .threads
            .iter()
            .find(|t| Arc::ptr_eq(t.thread_object(), thread_ko))
            .map(|t| (t.id(), t.process(), t.state()));
        let Some((thread_id, pid, state)) = lookup else {
            return;
        };
        if matches!(state, ThreadState::Terminated) {
            return;
        }

        if let Some(thread) = self.threads.get_mut(thread_id) {
            thread.set_state(ThreadState::Terminated);
        }
        signals.push_thread(thread_ko.clone(), exit_code);

        if let Some(process) = self.processes.get(pid)
            && process.decrement_thread_count()
        {
            signals.push_process(process.process_object().clone(), exit_code);
            self.pending_process_removals.push(pid);
        }
    }

    pub(crate) fn terminate_thread_ko(
        &mut self,
        thread_ko: &Arc<ThreadObject>,
        exit_code: i32,
    ) -> DeferredSignals {
        let mut signals = DeferredSignals::default();
        self.collect_thread_termination(thread_ko, exit_code, &mut signals);
        signals
    }

    /// Идемпотентно завершает все потоки процесса: каждый живой поток
    /// получает `THREAD_TERMINATED`, по достижении нуля - процесс
    /// получает `PROCESS_TERMINATED`. На процессе без живых потоков
    /// сразу поднимает `PROCESS_TERMINATED` и ставит в очередь на удаление.
    pub(crate) fn terminate_process_ko(
        &mut self,
        process_ko: &Arc<ProcessObject>,
        exit_code: i32,
    ) -> DeferredSignals {
        let mut signals = DeferredSignals::default();
        let pid_lookup = self
            .processes
            .iter()
            .find(|p| Arc::ptr_eq(p.process_object(), process_ko))
            .map(Process::id);
        let Some(pid) = pid_lookup else {
            return signals;
        };
        // Собираем снимок KO живых потоков под scheduler-lock'ом, чтобы
        // не держать одновременно &self и &mut self при итерации.
        let live: Vec<Arc<ThreadObject>> = self
            .threads
            .iter()
            .filter(|t| t.process() == pid && !matches!(t.state(), ThreadState::Terminated))
            .map(|t| t.thread_object().clone())
            .collect();
        if live.is_empty() {
            signals.push_process(process_ko.clone(), exit_code);
            if !self.pending_process_removals.contains(&pid) {
                self.pending_process_removals.push(pid);
            }
            return signals;
        }
        for ko in &live {
            self.collect_thread_termination(ko, exit_code, &mut signals);
        }
        signals
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
        let (prev_id, idle_id) = match self.current_cpu() {
            Some(cpu) => (cpu.current(), cpu.idle()),
            None => return ScheduleAction::None,
        };

        // Поток мог быть переведён в `Terminated` через `terminate_thread_ko`
        // пока стоял в ready_queue: пропускаем такие записи и берём следующую.
        let next_id = loop {
            let popped = self
                .current_cpu_mut()
                .and_then(|cpu| cpu.ready_queue_mut().pop_highest())
                .map(|(id, _)| id);
            match popped {
                Some(id) => {
                    let alive = self
                        .threads
                        .get(id)
                        .is_some_and(|t| !matches!(t.state(), ThreadState::Terminated));
                    if alive {
                        break id;
                    }
                }
                None => break idle_id,
            }
        };

        let cpu = self
            .current_cpu_mut()
            .expect("cpu still present after ready-queue drain");
        cpu.set_current(next_id);

        if let Some(next) = self.threads.get_mut(next_id) {
            next.set_state(ThreadState::Running);
            next.set_time_slice_left(self.time_slice_ticks);
        }

        // Dying thread больше не current - безопасно удалить процессы,
        // у которых thread_count достиг 0. Drop `Arc<AddressSpace>`
        // освободит mapper и его фреймы.
        self.cleanup_pending_process_removals(next_id);

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
            // Fallback на текущий CPU id используется в bootstrap фазе,
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

    /// Удаляет процессы, чей `thread_count` достиг 0, при условии что новый
    /// `current` thread не ссылается на удаляемый процесс. Если pid соответствует
    /// `next_id.process` - оставляем pending в очереди (другой thread того же
    /// процесса сейчас активен; реально невозможный случай, т.к. thread_count=0,
    /// но safety-net).
    fn cleanup_pending_process_removals(&mut self, next_id: ThreadId) {
        if self.pending_process_removals.is_empty() {
            return;
        }
        let active_process = self.threads.get(next_id).map(Thread::process);
        let mut idx = 0;
        while idx < self.pending_process_removals.len() {
            let pid = self.pending_process_removals[idx];
            if Some(pid) == active_process {
                idx += 1;
                continue;
            }
            self.pending_process_removals.swap_remove(idx);
            // Drop возвращает `Arc<AddressSpace>` - если последняя ссылка,
            // mapper Drop'ит свои фреймы.
            let _removed = self.processes.remove(pid);
        }
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
    let TrampolinePayload { entry } = *payload;
    <A::Cpu as ArchCpu>::enable_preemption();
    entry();
    kobject::thread_exit(0)
}
