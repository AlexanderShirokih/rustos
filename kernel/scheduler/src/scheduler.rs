#![allow(unsafe_code)]

use alloc::{boxed::Box, sync::Arc, vec::Vec};
use core::{marker::PhantomData, num::NonZeroUsize};

use capability::{
    Capability, DrainedHandles, HandleReservation, HandleTable, ProcessObject, Reply, ThreadObject,
};
use collections::{LockCell, MutexCell};
use memory::{
    AccessMask, MappingTag, MemFlags, MemoryRegion, PAGE_SIZE,
    frame_allocator::FrameAllocator,
    memory_mapper::{AddressSpaceFactory, AddressSpaceHandle},
    user_vm_allocator::UserVmAllocator,
    virtual_address::{PageAlignedVirtualAddress, VirtualAddress},
};

use super::{
    address_space::AddressSpace,
    arch::{
        ArchContext, ArchCpu, CpuId, CpuLocalError, MAX_CPUS, TimerSource, with_preemption_disabled,
    },
    cpu::Cpu,
    process::{Process, ProcessName, ProcessTable},
    thread::{IpcBufferSlot, Thread, ThreadState},
    thread_table::ThreadTable,
    wait_queue::{SleepEntry, SleepQueue},
};
use crate::{
    PreparedUserProcess, PreparedUserProcessError, Priority, ProcessId, SpawnAddressSpace,
    SpawnConfig, SpawnError, ThreadId, UserBootstrapArg, UserProcessLaunchInfo,
};

const DEFAULT_TIME_SLICE_TICKS: u32 = 1;
const IPC_BUFFER_PAGES: NonZeroUsize = NonZeroUsize::MIN;
/// Длина кванта по умолчанию (нс). Экспонируется для тестов.
pub const DEFAULT_QUANTUM_NS: u64 = 10_000_000;

/// Зарезервированный под стартовый хэндл слот child-таблицы вместе с процессом
/// и таблицей: `commit_reserved` идёт после всех способных упасть шагов спавна.
struct ChildHandleSlot {
    process_id: ProcessId,
    process_object: Arc<ProcessObject>,
    table: Arc<MutexCell<HandleTable>>,
    reservation: HandleReservation,
}

/// Снимок IPC-контекста текущего потока, собранный за один захват scheduler-лока
/// (см. [`SchedulerInner::current_ipc_parts`]).
pub(super) struct IpcContextParts {
    pub(super) address_space: Arc<AddressSpace>,
    pub(super) allocator: Arc<MutexCell<UserVmAllocator>>,
    pub(super) ipc_buffer_va: VirtualAddress,
    pub(super) handle_table: Arc<MutexCell<HandleTable>>,
}

#[derive(Clone, Copy, Debug)]
pub(super) enum AddressSpaceTransition {
    Keep,
    Switch(Option<AddressSpaceHandle>),
}

pub(super) enum ScheduleAction<A: ArchContext> {
    None,
    Switch {
        prev: *mut A,
        next: *const A,
        address_space: AddressSpaceTransition,
    },
}

/// Отложенное до выхода из-под scheduler-лока: пробуждения и дренаж таблиц
/// терминированных процессов - и то, и другое берёт тот же лок.
#[derive(Default)]
#[must_use = "DeferredSignals содержит отложенные пробуждения и дренаж таблиц"]
pub(super) struct DeferredSignals {
    threads: Vec<(Arc<ThreadObject>, i32)>,
    processes: Vec<Arc<ProcessObject>>,
    replies: Vec<Arc<Reply>>,
    handle_tables: Vec<Arc<MutexCell<HandleTable>>>,
}

impl DeferredSignals {
    fn push_thread(&mut self, thread: Arc<ThreadObject>, exit_code: i32) {
        self.threads.push((thread, exit_code));
    }

    fn push_process(&mut self, process: Arc<ProcessObject>) {
        self.processes.push(process);
    }

    fn push_reply(&mut self, reply: Arc<Reply>) {
        self.replies.push(reply);
    }

    fn push_handle_table(&mut self, table: Arc<MutexCell<HandleTable>>) {
        self.handle_tables.push(table);
    }

    pub(super) fn emit(self) {
        for (thread, exit_code) in self.threads {
            thread.signal_terminated(exit_code);
        }
        // Процессы помечены завершёнными под scheduler-локом, здесь остаётся
        // пробуждение наблюдателей.
        for process in self.processes {
            process.wake_terminated();
        }
        for reply in self.replies {
            reply.cancel();
        }
        for table in self.handle_tables {
            table.with_lock(HandleTable::drain).release();
        }
    }
}

/// Отложенное до выхода из-под scheduler-лока разрушение capability: и `Drop`
/// стартового хэндла откатанного спавна, и освобождение изъятого из таблицы
/// загрузчика будят ждущих через тот же лок.
#[derive(Default)]
#[must_use = "DeferredHandles содержит capability, ждущие разрушения вне лока"]
pub(super) struct DeferredHandles {
    rolled_back: Vec<Capability>,
    drained: Vec<DrainedHandles>,
}

impl DeferredHandles {
    /// Стартовый хэндл откатанного спавна: он никуда не встал, поэтому только
    /// роняется, без отзыва производных.
    fn push_rolled_back(&mut self, handle: Capability) {
        self.rolled_back.push(handle);
    }

    /// Изъятое из таблицы загрузчика: cancel-цели снятых слотов и остаток
    /// capability освобождает [`DrainedHandles::release`].
    fn push_drained(&mut self, drained: DrainedHandles) {
        self.drained.push(drained);
    }

    pub(super) fn emit(self) {
        drop(self.rolled_back);
        for drained in self.drained {
            drained.release();
        }
    }
}

// Ручные impls: derive требовал бы `A: Copy`, которого `ArchContext` не имеет.
impl<A: ArchContext> Clone for ScheduleAction<A> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<A: ArchContext> Copy for ScheduleAction<A> {}

/// Type-state маркеры состояния планировщика.
pub trait SchedulerStage {}

/// Планировщик создан, но per-CPU состояние ещё не установлено.
pub struct Uninit;
/// Per-CPU состояние установлено; доступен `spawn` и регистрация сервисов.
pub struct Bootstrapped;
/// Произошёл первый context switch; доступны yield, sleep, exit.
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
    /// Глобальный аллокатор фреймов для backing per-thread IPC-буферов.
    frame_allocator: Option<&'static (dyn FrameAllocator + Send + Sync)>,
    /// Процессы, у которых счётчик thread'ов достиг 0.
    pending_process_removals: Vec<ProcessId>,
    /// Снятые с таблицы процессы, ждущие разрушения вне scheduler-лока.
    pending_process_drops: Vec<Process>,
    /// Завершённые потоки, ждущие реапа из `ThreadTable`.
    pending_thread_removals: Vec<ThreadId>,
    /// Потоки, чей IPC-буфер ждёт освобождения: снимать отображение у потока,
    /// исполняющегося на другом CPU, значит выдернуть память из-под syscall.
    pending_ipc_releases: Vec<ThreadId>,
    /// Capability, ждущие разрушения вне scheduler-лока.
    deferred_handles: DeferredHandles,
    /// CPU, которым под локом добавили работу. Бит на CPU: отправка идёт вне
    /// лока, а там список уже не собрать.
    pending_wakeups: u16,
    quantum_ns: u64,
    time_slice_ticks: u32,
}

// Маска пробуждений держит по биту на CPU и потому связана с тем же пределом,
// что и `CpuId`.
const _: () = assert!(MAX_CPUS <= u16::BITS as usize);

/// Отказ подъёма планировщика на CPU.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CpuBootstrapError {
    /// Планировщик ещё не поднят: принимать CPU некому.
    NotReady,
    /// Планировщик на этом CPU уже поднят.
    AlreadyBootstrapped,
    /// Не удалось создать idle-поток этого CPU.
    IdleThread(SpawnError),
    /// Платформенный слой не принял per-CPU состояние.
    CpuLocal(CpuLocalError),
    /// Планировщику нечего запустить на этом CPU: нет даже idle-потока.
    NoRunnableThread,
    /// Таблица CPU не приняла слот под этот CPU.
    NoCpuSlot,
}

impl core::fmt::Display for CpuBootstrapError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            CpuBootstrapError::NotReady => f.write_str("the scheduler is not up yet"),
            CpuBootstrapError::AlreadyBootstrapped => {
                f.write_str("the scheduler is already running on this CPU")
            }
            CpuBootstrapError::IdleThread(e) => write!(f, "idle thread spawn failed: {e:?}"),
            CpuBootstrapError::CpuLocal(e) => write!(f, "per-CPU state was not installed: {e}"),
            CpuBootstrapError::NoRunnableThread => {
                f.write_str("no runnable thread to start on this CPU")
            }
            CpuBootstrapError::NoCpuSlot => f.write_str("no CPU table slot for this CPU"),
        }
    }
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

    /// `None` - режим без user-AS; `SpawnAddressSpace::User` вернёт `AddressSpaceCreationFailed`.
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
                frame_allocator: None,
                pending_process_removals: Vec::new(),
                pending_process_drops: Vec::new(),
                pending_thread_removals: Vec::new(),
                pending_ipc_releases: Vec::new(),
                deferred_handles: DeferredHandles::default(),
                pending_wakeups: 0,
                quantum_ns: DEFAULT_QUANTUM_NS,
                time_slice_ticks: DEFAULT_TIME_SLICE_TICKS,
            })),
            _stage: PhantomData,
        }
    }

    /// Создаёт per-CPU state, idle-поток и устанавливает CPU-local указатель.
    /// Должен вызываться при замаскированных IRQ. На отказе возвращает исходный
    /// планировщик: в `Bootstrapped` он не переходит.
    pub fn bootstrap(self) -> Result<Scheduler<A, T, Bootstrapped>, (Self, CpuBootstrapError)> {
        if let Err(e) = self.inner.with_lock(SchedulerInner::bootstrap_current_cpu) {
            return Err((self, e));
        }

        Ok(Scheduler {
            inner: self.inner,
            _stage: PhantomData::<Bootstrapped>,
        })
    }
}

impl<A, T> Scheduler<A, T, Bootstrapped>
where
    A: ArchContext,
    T: TimerSource,
{
    pub fn handle(&self) -> super::service::SchedulerHandle<A, T> {
        super::service::SchedulerHandle::new(self.inner.clone())
    }

    /// Спавн потока в bootstrap-фазе до первого context switch.
    pub fn spawn<F>(&self, cfg: SpawnConfig, entry: F) -> Result<ThreadId, SpawnError>
    where
        F: FnOnce() + Send + 'static,
    {
        self.inner.with_lock(|inner| inner.spawn(cfg, entry))
    }

    pub fn spawn_prepared_user_process(
        &self,
        prepared: PreparedUserProcess,
    ) -> Result<UserProcessLaunchInfo, PreparedUserProcessError> {
        let result = self
            .inner
            .with_lock(|inner| inner.spawn_prepared_user_process(prepared));
        release_deferred_handles(&self.inner);
        result
    }

    /// Выполняет первый switch через `A::switch`, возвращая управление вызывающему.
    pub fn run(self) -> Scheduler<A, T, Running> {
        let running = Scheduler {
            inner: self.inner,
            _stage: PhantomData::<Running>,
        };

        with_preemption_disabled::<A::Cpu, _>(|| {
            let action = running.inner.with_lock(|inner| {
                let now_ns = inner.timer.now_ns();
                inner.switch_to_next(now_ns)
            });
            perform_schedule_action(&running.inner, action);
        });

        running
    }

    /// Канонический boot-путь: выбирает первый поток и не возвращается. `Err(self)`
    /// - нет per-CPU state либо выбранного потока нет в таблице.
    pub fn start(self) -> Result<core::convert::Infallible, Self> {
        <A::Cpu as ArchCpu>::disable_preemption();
        let Some((next_ptr, address_space)) = self
            .inner
            .with_lock(SchedulerInner::prepare_first_thread_start)
        else {
            return Err(self);
        };
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
    /// Текущий поток. `None`, если per-CPU state ещё не установлен.
    pub fn current(&self) -> Option<ThreadId> {
        self.inner.with_lock(|inner| inner.current())
    }

    pub fn yield_now(&self) {
        with_preemption_disabled::<A::Cpu, _>(|| {
            let action = self.inner.with_lock(|inner| {
                let now_ns = inner.timer.now_ns();
                inner.yield_now(now_ns)
            });
            perform_schedule_action(&self.inner, action);
        });
    }

    pub fn sleep_ns(&self, ns: u64) {
        with_preemption_disabled::<A::Cpu, _>(|| {
            let action = self.inner.with_lock(|inner| {
                let now_ns = inner.timer.now_ns();
                inner.sleep_current(ns, now_ns)
            });
            perform_schedule_action(&self.inner, action);
        });
    }

    pub fn on_tick(&self, now_ns: u64) {
        let action = self.inner.with_lock(|inner| inner.on_tick(now_ns));
        perform_schedule_action(&self.inner, action);
    }

    /// Завершает текущий поток и переключается на следующий.
    pub fn exit_current(&self) {
        self.exit_current_with_code(0);
    }

    /// Тестовый вариант [`Self::exit_current`] с явным `exit_code`.
    #[doc(hidden)]
    pub fn exit_current_with_code(&self, exit_code: i32) {
        self.exit_current_interleaved(exit_code, || {});
    }

    /// Тестовый разрез [`Self::exit_current`]: `between` вызывается в окне
    /// между подготовкой переключения и его выполнением - там, где указатель
    /// на контекст уходящего потока живёт вне scheduler-лока.
    #[doc(hidden)]
    pub fn exit_current_interleaved(&self, exit_code: i32, between: impl FnOnce()) {
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
        between();
        perform_schedule_action(&self.inner, action);
    }

    pub fn handle(&self) -> super::service::SchedulerHandle<A, T> {
        super::service::SchedulerHandle::new(self.inner.clone())
    }

    pub fn spawn_prepared_user_process(
        &self,
        prepared: PreparedUserProcess,
    ) -> Result<UserProcessLaunchInfo, PreparedUserProcessError> {
        let result = self
            .inner
            .with_lock(|inner| inner.spawn_prepared_user_process(prepared));
        release_deferred_handles(&self.inner);
        result
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

    /// Регистрирует глобальный аллокатор фреймов для backing per-thread IPC-буферов.
    pub fn set_frame_allocator(
        &self,
        frame_allocator: &'static (dyn FrameAllocator + Send + Sync),
    ) {
        self.inner.with_lock(|inner| {
            inner.frame_allocator = Some(frame_allocator);
        });
    }

    /// Количество живых процессов в `ProcessTable`.
    pub fn process_count(&self) -> usize {
        self.inner.with_lock(|inner| inner.processes.live_count())
    }

    /// Количество живых потоков процесса `pid`; `None` если процесс не найден.
    pub fn process_thread_count(&self, pid: ProcessId) -> Option<usize> {
        self.inner
            .with_lock(|inner| inner.processes.get(pid).map(Process::thread_count))
    }

    /// Количество user-VM-регионов процесса `pid`; `None` если нет процесса или user-vm.
    pub fn process_user_vm_region_count(&self, pid: ProcessId) -> Option<usize> {
        use collections::LockCell;
        self.inner.with_lock(|inner| {
            let process = inner.processes.get(pid)?;
            let vm = process.user_vm()?.clone();
            Some(vm.with_lock(|alloc| alloc.live_count()))
        })
    }

    /// `Arc<ThreadObject>` потока `id`; `None` если поток не найден.
    pub fn thread_object_for(&self, id: ThreadId) -> Option<Arc<ThreadObject>> {
        self.inner
            .with_lock(|inner| inner.threads.get(id).map(|t| t.thread_object().clone()))
    }

    /// `Arc` handle-таблицы процесса `pid`; `None` если процесс не найден.
    pub fn process_handle_table(&self, pid: ProcessId) -> Option<Arc<MutexCell<HandleTable>>> {
        self.inner
            .with_lock(|inner| inner.processes.get(pid).map(|p| p.handle_table().clone()))
    }

    /// `Arc<ProcessObject>` процесса `pid`; `None` если процесс не найден.
    pub fn process_object_for(&self, pid: ProcessId) -> Option<Arc<ProcessObject>> {
        self.inner
            .with_lock(|inner| inner.processes.get(pid).map(|p| p.process_object().clone()))
    }

    /// `ProcessId` процесса, к которому привязан поток `id`; `None` если поток не найден.
    pub fn thread_process_id(&self, id: ThreadId) -> Option<ProcessId> {
        self.inner
            .with_lock(|inner| inner.threads.get(id).map(Thread::process))
    }

    /// `ProcessId` процесса по lifecycle-capability target; `None` если процесс не найден.
    pub fn process_id_for(&self, target: &Arc<ProcessObject>) -> Option<ProcessId> {
        self.inner.with_lock(|inner| {
            inner
                .processes
                .iter()
                .find(|p| Arc::ptr_eq(p.process_object(), target))
                .map(Process::id)
        })
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
        let target = self.pick_cpu();
        let priority = cfg.priority;
        let id = self.create_thread_on(cfg, entry, target)?;
        self.enqueue_ready(id, priority, target);
        Ok(id)
    }

    fn create_thread_on(
        &mut self,
        cfg: SpawnConfig,
        entry: Box<dyn FnOnce() + Send + 'static>,
        cpu_affinity: CpuId,
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
            Ok(thread) => thread.id(),
            Err(e) => {
                self.rollback_intern(process_id, is_new_process);
                return Err(e);
            }
        };

        Ok(id)
    }

    /// Создаёт per-CPU состояние планировщика на текущем CPU.
    ///
    /// Порядок шагов задан откатом: указатель отдаётся платформенному слою до
    /// того, как `Cpu` встанет в таблицу. Слот, заполненный для CPU, который в
    /// планировщик так и не вошёл, копил бы потоки в очереди, которую никто не
    /// разбирает.
    pub(super) fn bootstrap_current_cpu(&mut self) -> Result<(), CpuBootstrapError> {
        let cpu_id = <A::Cpu as ArchCpu>::current_id();
        if self.cpu_by_id(cpu_id).is_some() {
            return Err(CpuBootstrapError::AlreadyBootstrapped);
        }

        let idle_priority = lowest_priority(self.config.priority_levels());
        let time_slice_ticks = self.time_slice_ticks;
        let idle_id = self
            .create_thread_on(
                SpawnConfig::new("idle").priority(idle_priority),
                Box::new(|| <A::Cpu as ArchCpu>::idle()),
                cpu_id,
            )
            .map_err(CpuBootstrapError::IdleThread)?;

        let idle = self
            .threads
            .get_mut(idle_id)
            .ok_or(CpuBootstrapError::IdleThread(SpawnError::NoFreeThreadSlots))?;
        idle.set_state(ThreadState::Running);
        idle.set_time_slice_left(time_slice_ticks);

        let mut cpu = Box::new(Cpu::new(cpu_id, idle_id, self.config.priority_levels()));
        let cpu_ptr = core::ptr::from_mut::<Cpu>(&mut cpu).cast::<()>();

        // SAFETY: указатель ведёт в кучу, а не в `Box`, поэтому перемещение
        // `Box` в таблицу ниже его не сдвигает; сама таблица живёт столько же,
        // сколько планировщик.
        if let Err(e) = unsafe { <A::Cpu as ArchCpu>::install_cpu_local(cpu_ptr) } {
            self.drop_prepared_thread(idle_id);
            return Err(CpuBootstrapError::CpuLocal(e));
        }

        self.ensure_cpu_slot(cpu_id);
        let Some(slot) = self.cpus.get_mut(cpu_id.as_index()) else {
            // Опубликованный указатель ведёт в `Box`, который сейчас будет
            // уничтожен: снимаем его до возврата.
            // SAFETY: null - разрешённое значение CPU-local слота, оно же
            // стоит там до подъёма.
            let _ = unsafe { <A::Cpu as ArchCpu>::install_cpu_local(core::ptr::null_mut()) };
            self.drop_prepared_thread(idle_id);
            return Err(CpuBootstrapError::NoCpuSlot);
        };
        *slot = Some(cpu);

        Ok(())
    }

    fn pick_cpu(&self) -> CpuId {
        let current = self.current_cpu();
        let current_id = current.map_or_else(<A::Cpu as ArchCpu>::current_id, Cpu::id);
        // CPU без поднятого состояния в выборе не участвует, и его нагрузка
        // недостижимо велика: поставить поток некуда.
        let current_load = current.map_or(usize::MAX, Self::cpu_load);

        self.cpus
            .iter()
            .flatten()
            .filter(|cpu| Self::cpu_load(cpu) < current_load)
            .min_by_key(|cpu| Self::cpu_load(cpu))
            .map_or(current_id, |cpu| cpu.id())
    }

    /// Число потоков, за которые CPU отвечает: очередь готовых и исполняемый
    /// поток. Одной очереди мало - CPU, занятый потоком, опустошает её и по ней
    /// неотличим от простаивающего.
    fn cpu_load(cpu: &Cpu) -> usize {
        let running = usize::from(cpu.current() != cpu.idle());

        cpu.ready_queue().len() + running
    }

    fn enqueue_ready(&mut self, id: ThreadId, priority: Priority, target: CpuId) {
        let Some(cpu) = self.cpu_by_id_mut(target) else {
            return;
        };
        // Idle-поток в очереди не стоит: он выбирается, когда очередь пуста.
        if cpu.idle() == id {
            return;
        }

        cpu.ready_queue_mut().push(id, priority);
        self.mark_wakeup(target);
    }

    fn mark_wakeup(&mut self, target: CpuId) {
        if self.current_cpu().map(Cpu::id) == Some(target) {
            return;
        }

        self.pending_wakeups |= 1u16 << target.as_index();
    }

    pub(super) fn take_pending_wakeups(&mut self) -> u16 {
        core::mem::take(&mut self.pending_wakeups)
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
    ) -> Result<ProcessName<'static>, PreparedUserProcessError> {
        let name = ProcessName::new(prepared.name)
            .ok_or(PreparedUserProcessError::Spawn(SpawnError::InvalidName))?;
        if prepared.kernel_stack_pages == 0 {
            return Err(PreparedUserProcessError::Spawn(
                SpawnError::InvalidStackPages,
            ));
        }
        if (prepared.priority.raw() as usize) >= self.config.priority_levels() {
            return Err(PreparedUserProcessError::Spawn(SpawnError::InvalidPriority));
        }
        Ok(name)
    }

    /// Забирает отложенное разрушение capability: оно снимает учёт держателей
    /// порта и будит ждущих, а это берёт scheduler-лок.
    pub(super) fn take_deferred_handles(&mut self) -> DeferredHandles {
        core::mem::take(&mut self.deferred_handles)
    }

    /// Заводит процесс потомка с зарезервированным слотом под стартовый хэндл.
    /// `None` - слоты процессов или слоты child-таблицы исчерпаны.
    ///
    /// Заведённый здесь процесс откаты спавна роняют прямо под scheduler-локом,
    /// минуя карантин `pending_process_drops`. Пробуждения на разрушении процесса
    /// приходят только от capability его таблицы, а таблица потомка до
    /// `commit_reserved` - последнего шага спавна - держит максимум резервацию
    /// слота. Стартовый хэндл откаты отдают в `deferred_handles`, поэтому и он
    /// разрушается вне лока.
    fn insert_child_process(
        &mut self,
        name: ProcessName<'_>,
        address_space: &Arc<AddressSpace>,
        user_vm: Option<UserVmAllocator>,
    ) -> Option<ChildHandleSlot> {
        let address_space = address_space.clone();
        let process = self
            .processes
            .insert_with(|id| {
                let mut process = Process::new(id, name, address_space);
                if let Some(vm) = user_vm {
                    process = process.with_user_vm(vm);
                }
                process
            })
            .ok()?;
        let process_id = process.id();
        let process_object = process.process_object().clone();
        let table = process.handle_table().clone();

        let Ok(reservation) = table.with_lock(HandleTable::reserve_slot) else {
            self.processes.remove(process_id);
            return None;
        };
        Some(ChildHandleSlot {
            process_id,
            process_object,
            table,
            reservation,
        })
    }

    pub(crate) fn spawn_prepared_user_process(
        &mut self,
        prepared: PreparedUserProcess,
    ) -> Result<UserProcessLaunchInfo, PreparedUserProcessError> {
        let name = match self.validate_prepared_user_process(&prepared) {
            Ok(name) => name,
            Err(e) => {
                self.deferred_handles
                    .push_rolled_back(prepared.launch.initial_handle);
                return Err(e);
            }
        };
        let launch = prepared.launch;
        let initial_handle = launch.initial_handle;

        let Ok(stack) =
            <A::Stack as super::arch::ThreadStackAllocator>::allocate(prepared.kernel_stack_pages)
        else {
            self.deferred_handles.push_rolled_back(initial_handle);
            return Err(PreparedUserProcessError::Spawn(
                SpawnError::StackAllocationFailed,
            ));
        };
        let stack_top = stack.top();

        let Some(ChildHandleSlot {
            process_id,
            process_object,
            table: child_table,
            reservation,
        }) = self.insert_child_process(name, &prepared.address_space, prepared.user_vm)
        else {
            self.deferred_handles.push_rolled_back(initial_handle);
            return Err(PreparedUserProcessError::Spawn(
                SpawnError::NoFreeThreadSlots,
            ));
        };
        let initial_handle_id = reservation.handle_id();
        let bootstrap_arg = UserBootstrapArg(u64::from(initial_handle_id.raw().get()));

        let arch = A::init_user(crate::UserEntry {
            kernel_stack_top: stack_top,
            user_pc: prepared.user_pc,
            user_sp: prepared.user_sp,
            arg: bootstrap_arg,
        });

        let cpu_affinity = self.pick_cpu();

        let (thread_id, thread_object) = match self.threads.insert_with(|id| {
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
            Ok(thread) => (thread.id(), thread.thread_object().clone()),
            Err(e) => {
                // Откат: без потока thread_count=1 никогда не обнулится и AS не освободится.
                self.processes.remove(process_id);
                self.deferred_handles.push_rolled_back(initial_handle);
                return Err(PreparedUserProcessError::Spawn(e));
            }
        };

        if let Err(e) = self.attach_ipc_buffer(process_id, thread_id) {
            self.threads.remove(thread_id);
            self.processes.remove(process_id);
            self.deferred_handles.push_rolled_back(initial_handle);
            return Err(PreparedUserProcessError::Spawn(e));
        }

        let mut rejected = DrainedHandles::empty();
        child_table
            .with_lock(|tbl| tbl.commit_reserved(reservation, initial_handle, &mut rejected));
        self.deferred_handles.push_drained(rejected);

        self.enqueue_ready(thread_id, prepared.priority, cpu_affinity);

        Ok(UserProcessLaunchInfo {
            process_id,
            thread_id,
            initial_handle_id,
            process_object,
            thread_object,
        })
    }

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
        let name = ProcessName::new(name).ok_or(SpawnError::InvalidName)?;
        let pid = self
            .processes
            .insert(name, address_space)
            .map_err(|_| SpawnError::NoFreeThreadSlots)?;
        Ok((pid, true))
    }

    /// Снятый процесс роняется прямо под scheduler-локом, минуя карантин
    /// `pending_process_drops`: заведён он этим же спавном, хэндлов не получал,
    /// и его разрушение никого не будит.
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

    /// Перевыбор потока по требованию другого CPU.
    ///
    /// В отличие от тика, квант текущего потока не расходуется: сообщение
    /// говорит лишь о появившейся работе, а времени с прошлого тика не прошло.
    /// Поэтому вытеснение только в пользу более приоритетного - и всегда, если
    /// CPU стоял в idle.
    pub(super) fn reschedule(&mut self, now_ns: u64) -> ScheduleAction<A> {
        let Some((current_id, idle_id, highest_ready)) = self.current_cpu().map(|cpu| {
            (
                cpu.current(),
                cpu.idle(),
                cpu.ready_queue().peek_highest_priority(),
            )
        }) else {
            return ScheduleAction::None;
        };

        let Some(highest_ready) = highest_ready else {
            return ScheduleAction::None;
        };

        let should_preempt = current_id == idle_id
            || self
                .threads
                .get(current_id)
                .is_some_and(|current| highest_ready < current.priority());

        if should_preempt {
            self.preempt_current(now_ns)
        } else {
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
        if let Some(current_id) = self.current()
            && let Some(thread) = self.threads.get_mut(current_id)
        {
            thread.set_state(ThreadState::Sleeping { wakeup_at_ns });
            thread.set_time_slice_left(self.time_slice_ticks);
            self.sleepers.push(SleepEntry {
                wakeup_at_ns,
                thread_id: current_id,
            });
        }

        self.switch_to_next(now_ns)
    }

    pub(super) fn block_current(
        &mut self,
        now_ns: u64,
        timeout_ns: Option<u64>,
    ) -> ScheduleAction<A> {
        if let Some(current_id) = self.current()
            && let Some(thread) = self.threads.get_mut(current_id)
        {
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
                self.enqueue_ready(id, priority, affinity);
            }
            _ => {}
        }
    }

    pub(super) fn current_handle_table(&self) -> Option<Arc<MutexCell<HandleTable>>> {
        let current_id = self.current_cpu()?.current();
        let pid = self.threads.get(current_id)?.process();
        self.processes.get(pid).map(|p| p.handle_table().clone())
    }

    pub(super) fn current_thread_object(&self) -> Option<Arc<ThreadObject>> {
        let current_id = self.current_cpu()?.current();
        self.threads
            .get(current_id)
            .map(|t| t.thread_object().clone())
    }

    pub(super) fn current_process_object(&self) -> Option<Arc<ProcessObject>> {
        let current_id = self.current_cpu()?.current();
        let pid = self.threads.get(current_id)?.process();
        self.processes.get(pid).map(|p| p.process_object().clone())
    }

    pub(super) fn current_user_vm_pair(
        &self,
    ) -> Option<(Arc<AddressSpace>, Arc<MutexCell<UserVmAllocator>>)> {
        let current_id = self.current_cpu()?.current();
        let pid = self.threads.get(current_id)?.process();
        let process = self.processes.get(pid)?;
        let vm = process.user_vm()?.clone();
        Some((process.address_space().clone(), vm))
    }

    /// Снимок IPC-контекста текущего потока одним обращением: адресное
    /// пространство, user-VM, VA IPC-буфера и handle-таблица его процесса.
    pub(super) fn current_ipc_parts(&self) -> Option<IpcContextParts> {
        let current_id = self.current_cpu()?.current();
        let thread = self.threads.get(current_id)?;
        let ipc_buffer_va = thread.ipc_buffer_va()?.as_virtual();
        let process = self.processes.get(thread.process())?;
        Some(IpcContextParts {
            address_space: process.address_space().clone(),
            allocator: process.user_vm()?.clone(),
            ipc_buffer_va,
            handle_table: process.handle_table().clone(),
        })
    }

    pub(super) fn current(&self) -> Option<ThreadId> {
        Some(self.current_cpu()?.current())
    }

    pub(super) fn now_ns(&self) -> u64 {
        self.timer.now_ns()
    }

    pub(super) fn begin_exit_current(&mut self, exit_code: i32) -> DeferredSignals {
        let mut signals = DeferredSignals::default();
        let Some(current_id) = self.current() else {
            return signals;
        };
        let (exiting_pid, thread_object) = if let Some(thread) = self.threads.get_mut(current_id) {
            thread.set_state(ThreadState::Terminated);
            (Some(thread.process()), Some(thread.thread_object().clone()))
        } else {
            (None, None)
        };

        if let Some(target) = thread_object {
            signals.push_thread(target, exit_code);
        }

        // На нуле поднимаем terminated процесса ДО `cleanup_pending_process_removals`
        // (вызов из `switch_to_next`); `Arc<ProcessObject>` переживёт удаление из таблицы.
        if let Some(pid) = exiting_pid
            && self
                .processes
                .get(pid)
                .is_some_and(Process::decrement_thread_count)
        {
            self.collect_process_termination(pid, exit_code, &mut signals);
        }

        // Kernel-стек освободится в `reap_pending_threads`, когда поток перестанет быть prev.
        self.release_ipc_buffer(current_id);
        self.pending_thread_removals.push(current_id);

        signals
    }

    pub(super) fn finish_exit_current(&mut self, now_ns: u64) -> ScheduleAction<A> {
        self.switch_to_next(now_ns)
    }

    pub(crate) fn create_empty_process(
        &mut self,
        name: &str,
    ) -> Result<Arc<ProcessObject>, SpawnError> {
        let name = ProcessName::new(name).ok_or(SpawnError::InvalidName)?;
        let factory = self
            .address_space_factory
            .ok_or(SpawnError::AddressSpaceCreationFailed)?;
        let address_space =
            AddressSpace::new_user(factory).map_err(|_| SpawnError::AddressSpaceCreationFailed)?;
        let process = self
            .processes
            .insert_with(|id| Process::empty(id, name, address_space))
            .map_err(|_| SpawnError::NoFreeThreadSlots)?;
        Ok(process.process_object().clone())
    }

    pub(crate) fn create_user_thread(
        &mut self,
        process_object: &Arc<ProcessObject>,
        entry: capability::UserThreadEntry,
    ) -> Result<Arc<ThreadObject>, SpawnError> {
        let (thread_id, thread_object) = self.prepare_user_thread(process_object, entry)?;
        self.enqueue_user_thread_ready(thread_id);
        Ok(thread_object)
    }

    pub(crate) fn prepare_user_thread(
        &mut self,
        process_object: &Arc<ProcessObject>,
        entry: capability::UserThreadEntry,
    ) -> Result<(ThreadId, Arc<ThreadObject>), SpawnError> {
        // Терминация необратима: снимаемый процесс новых потоков не получает.
        if process_object.terminated() {
            return Err(SpawnError::ProcessTerminated);
        }
        if (entry.priority as usize) >= self.config.priority_levels() {
            return Err(SpawnError::InvalidPriority);
        }

        let priority = Priority::new(entry.priority);
        let process_id = self
            .processes
            .iter()
            .find(|p| Arc::ptr_eq(p.process_object(), process_object))
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
            SpawnConfig::DEFAULT_STACK_PAGES,
        )
        .map_err(|_| SpawnError::StackAllocationFailed)?;
        let stack_top = stack.top();

        let arch = A::init_user(crate::UserEntry {
            kernel_stack_top: stack_top,
            user_pc: VirtualAddress::new(entry.entry_pc as usize),
            user_sp: VirtualAddress::new(entry.user_sp as usize),
            arg: UserBootstrapArg(entry.arg),
        });

        let cpu_affinity = self.pick_cpu();

        // Инкремент до insert: на ошибке компенсируем декремент без сигнала terminated.
        let process = self
            .processes
            .get(process_id)
            .ok_or(SpawnError::NoFreeThreadSlots)?;
        process.increment_thread_count();

        let (thread_id, thread_object) = match self.threads.insert_with(|id| {
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
            Ok(thread) => (thread.id(), thread.thread_object().clone()),
            Err(e) => {
                self.undo_thread_count(process_id);
                return Err(e);
            }
        };

        if let Err(e) = self.attach_ipc_buffer(process_id, thread_id) {
            self.threads.remove(thread_id);
            self.undo_thread_count(process_id);
            return Err(e);
        }

        Ok((thread_id, thread_object))
    }

    fn undo_thread_count(&mut self, process_id: ProcessId) {
        if let Some(process) = self.processes.get(process_id) {
            let _ = process.decrement_thread_count();
        }
    }

    fn attach_ipc_buffer(
        &mut self,
        process_id: ProcessId,
        thread_id: ThreadId,
    ) -> Result<(), SpawnError> {
        let Some(fa) = self.frame_allocator else {
            return Ok(());
        };

        let Some(process) = self.processes.get(process_id) else {
            return Ok(());
        };
        let Some(user_vm) = process.user_vm() else {
            return Ok(());
        };
        let user_vm = user_vm.clone();
        let mapper_arc = process
            .address_space()
            .mapper_arc()
            .ok_or(SpawnError::AddressSpaceCreationFailed)?;

        let region = MemoryRegion::create_virtual(fa, IPC_BUFFER_PAGES, AccessMask::RW)
            .map_err(|_| SpawnError::StackAllocationFailed)?;
        let region = Arc::new(region);
        let flags = MemFlags::user_rw();
        let grant = region.access_mask();

        let size = PAGE_SIZE;
        let allocated = user_vm
            .with_lock(|alloc| {
                alloc.allocate(
                    size,
                    MappingTag {
                        flags,
                        region: region.clone(),
                        grant,
                        // Стек потока не имеет публичной капы - отзывать нечего.
                        revocation: None,
                    },
                )
            })
            .map_err(|_| SpawnError::StackAllocationFailed)?;
        let base = allocated.base();

        if region.install(&*mapper_arc, base, flags).is_err() {
            user_vm.with_lock(|alloc| {
                let _ = alloc.free(base, size);
            });
            return Err(SpawnError::StackAllocationFailed);
        }

        let slot = IpcBufferSlot::new(base, region);
        let Some(thread) = self.threads.get_mut(thread_id) else {
            // Регион не должен остаться висеть в user-AS без владельца.
            debug_assert!(false, "ipc-buffer target thread must exist");
            user_vm.with_lock(|alloc| {
                let _ = alloc.free(base, size);
            });
            return Err(SpawnError::StackAllocationFailed);
        };
        thread.set_ipc_buffer(slot);
        Ok(())
    }

    /// БЕЗОПАСНОСТЬ: для потока, заблокированного на Port, caller обязан снять
    /// `Waiter` с очереди Port до вызова - иначе матчер пишет в уже снятый VA.
    fn release_ipc_buffer(&mut self, thread_id: ThreadId) {
        let Some(thread) = self.threads.get(thread_id) else {
            return;
        };
        let Some(base) = thread.ipc_buffer_va() else {
            return;
        };
        let pid = thread.process();

        let resources = self.processes.get(pid).map(|process| {
            (
                process.address_space().mapper_arc(),
                process.user_vm().cloned(),
            )
        });
        if let Some((mapper, user_vm)) = resources {
            let size = PAGE_SIZE;
            // Фрейм возвращается аллокатору только если PTE снят: живой PTE
            // после "освобождения" фрейма = UAF-поверхность. NotMapped штатен
            // (идемпотентный повторный вызов / уже снятый AS); любая другая
            // ошибка unmap (block-split, misalign) - баг построения.
            let unmapped = match mapper {
                Some(mapper) => match mapper.unmap(base, PAGE_SIZE.get()) {
                    Ok(()) | Err(memory::memory_mapper::MemoryUnmappingError::NotMapped) => true,
                    Err(_e) => {
                        debug_assert!(false, "unexpected unmap error for ipc-buffer");
                        false
                    }
                },
                None => true,
            };
            if unmapped && let Some(user_vm) = user_vm {
                user_vm.with_lock(|alloc| {
                    let _ = alloc.free(base, size);
                });
            }
        }

        if let Some(thread) = self.threads.get_mut(thread_id) {
            let _ = thread.take_ipc_buffer();
        }
    }

    fn reap_pending_threads(&mut self) {
        let mut idx = 0;
        while idx < self.pending_thread_removals.len() {
            let tid = self.pending_thread_removals[idx];
            if self.is_thread_held(tid) {
                idx += 1;
                continue;
            }
            self.pending_thread_removals.swap_remove(idx);
            let _ = self.threads.remove(tid);
        }
    }

    fn held_threads(&self) -> impl Iterator<Item = ThreadId> + '_ {
        self.cpus
            .iter()
            .flatten()
            .flat_map(|cpu| [Some(cpu.current()), cpu.previous()])
            .flatten()
    }

    fn is_thread_held(&self, id: ThreadId) -> bool {
        self.held_threads().any(|held| held == id)
    }

    fn release_previous(&mut self) {
        if let Some(cpu) = self.current_cpu_mut() {
            cpu.set_previous(None);
        }
        self.release_pending_ipc_buffers();
        self.reap_pending_threads();
        self.cleanup_pending_process_removals();
    }

    fn release_pending_ipc_buffers(&mut self) {
        let mut idx = 0;
        while idx < self.pending_ipc_releases.len() {
            let tid = self.pending_ipc_releases[idx];
            if self.is_thread_held(tid) {
                idx += 1;
                continue;
            }
            self.pending_ipc_releases.swap_remove(idx);
            self.release_ipc_buffer(tid);
        }
    }

    /// Закрывает переключение и забирает снятые процессы для разрушения вне
    /// scheduler-лока: их `Drop` берёт тот же лок.
    pub(super) fn finish_switch(&mut self) -> Vec<Process> {
        self.release_previous();
        core::mem::take(&mut self.pending_process_drops)
    }

    fn is_process_running(&self, pid: ProcessId) -> bool {
        self.held_threads()
            .filter_map(|id| self.threads.get(id))
            .any(|thread| thread.process() == pid)
    }

    pub(crate) fn set_current_blocked_cancel(&mut self, cancel: Arc<dyn capability::CancelTarget>) {
        if let Some(current_id) = self.current()
            && let Some(thread) = self.threads.get_mut(current_id)
        {
            thread.set_blocked_cancel(cancel);
        }
    }

    pub(crate) fn clear_current_blocked_cancel(&mut self) {
        if let Some(current_id) = self.current()
            && let Some(thread) = self.threads.get_mut(current_id)
        {
            let _ = thread.take_blocked_cancel();
        }
    }

    pub(super) fn current_ipc_buffer_va(&self) -> Option<VirtualAddress> {
        let current_id = self.current_cpu()?.current();
        Some(self.threads.get(current_id)?.ipc_buffer_va()?.as_virtual())
    }

    pub(crate) fn enqueue_user_thread_ready(&mut self, thread_id: ThreadId) {
        let Some(thread) = self.threads.get(thread_id) else {
            return;
        };
        let priority = thread.priority();
        let cpu_affinity = thread.cpu_affinity();

        self.enqueue_ready(thread_id, priority, cpu_affinity);
    }

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

    pub(crate) fn load_user_image_into(
        &mut self,
        process_object: &Arc<ProcessObject>,
        install: &capability::UserImageInstall,
    ) -> Result<(), capability::LoadImageError> {
        use capability::LoadImageError;

        let process = self
            .process_by_object_mut(process_object)
            .ok_or(LoadImageError::ProcessNotFound)?;
        if process.is_image_loaded()
            || process.thread_count() != 0
            || !process.handle_table_is_empty()
        {
            return Err(LoadImageError::WrongState);
        }

        // До маппинга: невалидный образ не должен оставлять процесс полузагруженным.
        install.validate_geometry()?;

        let mapper_arc = process
            .address_space()
            .mapper_arc()
            .ok_or(LoadImageError::NoUserAddressSpace)?;
        let mapper: &(dyn memory::memory_mapper::MemoryMapper + Send + Sync) = &*mapper_arc;

        // Overflow до маппинга: set_user_vm идёт последним, частичное состояние недопустимо.
        let user_vm_end_usize = install
            .user_vm_base
            .as_usize()
            .checked_add(install.user_vm_size)
            .ok_or(LoadImageError::UserVmRangeOverflow)?;

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

        let vm = UserVmAllocator::new(install.user_vm_base, VirtualAddress::new(user_vm_end_usize));
        process
            .set_user_vm(vm)
            .map_err(|_| LoadImageError::WrongState)?;
        // Регионы образа держит процесс: их бюджет возвращается вызывающему,
        // когда процесс уходит. Стек - такой же сегмент.
        let segments: Vec<Arc<MemoryRegion>> =
            install.segments.iter().map(|s| s.region.clone()).collect();
        process.set_image_segments(segments);
        Ok(())
    }

    pub(crate) fn start_user_process(
        &mut self,
        process_object: &Arc<ProcessObject>,
        spec: capability::UserStartSpec,
    ) -> Result<Arc<ThreadObject>, capability::StartProcessError> {
        use capability::StartProcessError;

        let capability::UserStartSpec {
            mut entry,
            loader_handle_table,
            handle_id,
            metering_grant,
        } = spec;

        let child_table = {
            let Some(process) = self.process_by_object_mut(process_object) else {
                return Err(StartProcessError::ProcessNotFound);
            };
            if process_object.terminated()
                || !process.is_image_loaded()
                || process.thread_count() != 0
                || !process.handle_table_is_empty()
            {
                return Err(StartProcessError::WrongState);
            }
            process.handle_table().clone()
        };

        // Validate хэндл под loader-lock'ом до drain: existence + TRANSFER.
        if let Err(e) = loader_handle_table
            .with_lock(|tbl| tbl.get(handle_id, capability::Rights::TRANSFER).map(|_| ()))
        {
            return Err(StartProcessError::HandleValidationFailed(e));
        }

        // Резерв child-слота до prepare фиксирует child-table id потомка, не
        // трогая loader-handle: на ошибке prepare откат - только release.
        let reservation = match child_table.with_lock(HandleTable::reserve_slot) {
            Ok(reservation) => reservation,
            Err(e) => return Err(StartProcessError::HandleValidationFailed(e)),
        };

        // X0 потомка - child-table id стартового хэндла-канала.
        entry.arg = u64::from(reservation.handle_id().raw().get());

        let (thread_id, thread_object) = match self.prepare_user_thread(process_object, entry) {
            Ok(t) => t,
            Err(source) => {
                child_table.with_lock(|tbl| tbl.release_reservation(reservation));
                return Err(StartProcessError::SpawnFailed(source.into()));
            }
        };

        // Validate и drain в разных with_lock-окнах; на гонке (другой syscall
        // закрыл handle между шагами) откатываем prepared thread и резервацию.
        // Изъятое уходит в отложенные: пробуждение cancel-целей снятого слота
        // берёт scheduler-лок, который держится здесь.
        let mut drained = DrainedHandles::empty();
        let taken = loader_handle_table.with_lock(|tbl| {
            tbl.try_drain_for_transfer(&[handle_id], capability::Rights::TRANSFER, &mut drained)
        });
        // На отказе снятое остаётся в токене: взятая наружу капа была бы
        // разрушена под scheduler-локом.
        let handle = match taken {
            Ok(()) => drained.take_handles().into_iter().next(),
            Err(_) => None,
        };
        self.deferred_handles.push_drained(drained);
        if let Err(e) = taken {
            self.drop_prepared_thread(thread_id);
            child_table.with_lock(|tbl| tbl.release_reservation(reservation));
            return Err(StartProcessError::HandleValidationFailed(e));
        }

        // commit_reserved кладёт хэндл в зарезервированный слот: child-table id
        // совпадает с уже вычисленным для X0.
        let Some(handle) = handle else {
            child_table.with_lock(|tbl| tbl.release_reservation(reservation));
            self.drop_prepared_thread(thread_id);
            return Err(StartProcessError::HandleValidationFailed(
                capability::IpcError::BadHandle,
            ));
        };
        let mut rejected = DrainedHandles::empty();
        child_table.with_lock(|tbl| tbl.commit_reserved(reservation, handle, &mut rejected));
        self.deferred_handles.push_drained(rejected);

        // До enqueue: стартовый поток не должен аллоцировать раньше засева.
        if let Some(grant) = metering_grant {
            process_object.set_metering(grant);
        }

        self.enqueue_user_thread_ready(thread_id);

        Ok(thread_object)
    }

    fn process_by_object_mut(&mut self, target: &Arc<ProcessObject>) -> Option<&mut Process> {
        self.processes
            .iter_mut_internal()
            .find(|p| Arc::ptr_eq(p.process_object(), target))
    }

    fn collect_thread_termination(
        &mut self,
        thread_object: &Arc<ThreadObject>,
        exit_code: i32,
        signals: &mut DeferredSignals,
    ) {
        let lookup = self
            .threads
            .iter()
            .find(|t| Arc::ptr_eq(t.thread_object(), thread_object))
            .map(|t| (t.id(), t.process(), t.state()));
        let Some((thread_id, pid, state)) = lookup else {
            return;
        };
        if matches!(state, ThreadState::Terminated) {
            return;
        }

        let blocked_cancel = self.threads.get_mut(thread_id).and_then(|thread| {
            thread.set_state(ThreadState::Terminated);
            thread.take_blocked_cancel()
        });
        signals.push_thread(thread_object.clone(), exit_code);

        // Cancel снимает Waiter с очереди и переводит OutcomeSlot в TIMEDOUT,
        // отклоняя поздний reply. Thread не реапим: стек заморожен в syscall и держит Arc'и.
        // Доставленный, но не изъятый reply забирает тот же cancel: в
        // замороженном кадре он стал бы недостижим, а вызыватель - вечным.
        if let Some(cancel) = blocked_cancel {
            cancel.cancel();
            if let Some(reply) = cancel.take_abandoned_reply() {
                signals.push_reply(reply);
            }
        }
        if self.is_thread_held(thread_id) {
            self.pending_ipc_releases.push(thread_id);
        } else {
            self.release_ipc_buffer(thread_id);
        }

        if self
            .processes
            .get(pid)
            .is_some_and(Process::decrement_thread_count)
        {
            self.collect_process_termination(pid, exit_code, signals);
        }
    }

    /// Терминация процесса: пометка завершения, сигнал наблюдателям, дренаж
    /// handle-таблицы и постановка в очередь на снятие из `ProcessTable`.
    /// Дренаж и постановка - один раз на процесс, сигнал переживает повторный
    /// вызов. Пометка идёт под scheduler-локом: её же читают отказы
    /// `ProcessStart`/`ThreadCreate`, а дренаж и пробуждения отложены.
    fn collect_process_termination(
        &mut self,
        pid: ProcessId,
        exit_code: i32,
        signals: &mut DeferredSignals,
    ) {
        let already_pending = self.pending_process_removals.contains(&pid);
        let Some(process) = self.processes.get(pid) else {
            return;
        };
        let process_object = process.process_object().clone();
        process_object.mark_terminated(exit_code);
        signals.push_process(process_object);
        if already_pending {
            return;
        }
        signals.push_handle_table(process.handle_table().clone());
        self.pending_process_removals.push(pid);
    }

    pub(crate) fn terminate_thread_object(
        &mut self,
        thread_object: &Arc<ThreadObject>,
        exit_code: i32,
    ) -> DeferredSignals {
        let mut signals = DeferredSignals::default();
        self.collect_thread_termination(thread_object, exit_code, &mut signals);
        signals
    }

    pub(crate) fn terminate_process_object(
        &mut self,
        process_object: &Arc<ProcessObject>,
        exit_code: i32,
    ) -> DeferredSignals {
        let mut signals = DeferredSignals::default();
        let pid_lookup = self
            .processes
            .iter()
            .find(|p| Arc::ptr_eq(p.process_object(), process_object))
            .map(Process::id);
        let Some(pid) = pid_lookup else {
            return signals;
        };
        // Собираем снимок capability target живых потоков под scheduler-lock'ом, чтобы
        // не держать одновременно &self и &mut self при итерации.
        let live: Vec<Arc<ThreadObject>> = self
            .threads
            .iter()
            .filter(|t| t.process() == pid && !matches!(t.state(), ThreadState::Terminated))
            .map(|t| t.thread_object().clone())
            .collect();
        if live.is_empty() {
            self.collect_process_termination(pid, exit_code, &mut signals);
            return signals;
        }
        for target in &live {
            self.collect_thread_termination(target, exit_code, &mut signals);
        }
        signals
    }

    fn wake_sleepers(&mut self, now_ns: u64) {
        // pop_if снимает голову очереди только когда её дедлайн наступил.
        while let Some(entry) = self.sleepers.pop_if(|entry| entry.wakeup_at_ns <= now_ns) {
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

            self.enqueue_ready(entry.thread_id, priority, affinity);
        }
    }

    fn preempt_current(&mut self, now_ns: u64) -> ScheduleAction<A> {
        let Some(cpu) = self.current_cpu() else {
            return ScheduleAction::None;
        };
        let (current_id, idle_id) = (cpu.current(), cpu.idle());

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

        // Поток мог быть переведён в `Terminated` через `terminate_thread_object`
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

        let Some(cpu) = self.current_cpu_mut() else {
            return ScheduleAction::None;
        };
        cpu.set_current(next_id);
        // Взводится до выхода из-под лока: ниже из `prev` берётся указатель,
        // который переживёт лок, и до конца переключения поток снимать нельзя.
        cpu.set_previous(Some(prev_id));

        if let Some(next) = self.threads.get_mut(next_id) {
            next.set_state(ThreadState::Running);
            next.set_time_slice_left(self.time_slice_ticks);
        }

        self.cleanup_pending_process_removals();
        self.reap_pending_threads();

        if prev_id == next_id {
            self.release_previous();
            self.schedule_next_deadline(now_ns);
            return ScheduleAction::None;
        }

        let from_proc = self.threads.get(prev_id).map(Thread::process);
        let to_proc = self.threads.get(next_id).map(Thread::process);
        let address_space = self.address_space_change(from_proc, to_proc);

        let Some((prev, next)) = self.threads.split_pair_mut(prev_id, next_id) else {
            self.release_previous();
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

    pub(super) fn prepare_first_thread_start(
        &mut self,
    ) -> Option<(*const A, Option<AddressSpaceHandle>)> {
        let now_ns = self.timer.now_ns();
        let cpu = self.current_cpu_mut()?;
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

        let time_slice_ticks = self.time_slice_ticks;
        let next_ptr = {
            let next = self.threads.get_mut(next_id)?;
            next.set_state(ThreadState::Running);
            next.set_time_slice_left(time_slice_ticks);
            core::ptr::from_ref(next.arch())
        };
        self.schedule_next_deadline(now_ns);
        Some((next_ptr, address_space))
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

    fn cpu_by_id(&self, id: CpuId) -> Option<&Cpu> {
        self.cpus
            .get(id.as_index())
            .and_then(|slot| slot.as_deref())
    }

    fn cpu_by_id_mut(&mut self, id: CpuId) -> Option<&mut Cpu> {
        self.cpus
            .get_mut(id.as_index())
            .and_then(|slot| slot.as_deref_mut())
    }

    fn cleanup_pending_process_removals(&mut self) {
        if self.pending_process_removals.is_empty() {
            return;
        }
        let mut idx = 0;
        while idx < self.pending_process_removals.len() {
            let pid = self.pending_process_removals[idx];
            if self.is_process_running(pid) {
                idx += 1;
                continue;
            }
            self.pending_process_removals.swap_remove(idx);
            if let Some(removed) = self.processes.remove(pid) {
                self.pending_process_drops.push(removed);
            }
        }
    }

    /// Длина таблицы CPU ограничена сверху: `CpuId` не выходит за `MAX_CPUS`.
    fn ensure_cpu_slot(&mut self, id: CpuId) {
        let required_len = id.as_index() + 1;
        if self.cpus.len() < required_len {
            self.cpus.resize_with(required_len, || None);
        }
    }
}

struct TrampolinePayload {
    entry: Box<dyn FnOnce() + Send + 'static>,
}

/// Разрушает отложенные capability уже вне scheduler-лока: их `Drop`, отзыв и
/// пробуждение cancel-целей берут тот же лок.
pub(super) fn release_deferred_handles<A: ArchContext, T: TimerSource>(
    inner: &MutexCell<SchedulerInner<A, T>>,
) {
    inner
        .with_lock(SchedulerInner::take_deferred_handles)
        .emit();
    notify_pending_cpus(inner);
}

/// Будит CPU, которым под локом добавили работу.
///
/// Вне лока: отправка идёт через контроллер прерываний, а тот берёт свои
/// локи - брать их из-под scheduler-лока значит задать двум локам порядок,
/// обратный тому, в котором их берёт обработчик прерывания.
pub(super) fn notify_pending_cpus<A: ArchContext, T: TimerSource>(
    inner: &MutexCell<SchedulerInner<A, T>>,
) {
    let mut pending = inner.with_lock(SchedulerInner::take_pending_wakeups);

    while pending != 0 {
        let index = pending.trailing_zeros();
        pending &= pending - 1;

        // Бит выставлен по `CpuId`, поэтому обратное преобразование полное.
        if let Ok(raw) = u16::try_from(index)
            && let Some(target) = CpuId::new(raw)
        {
            <A::Cpu as ArchCpu>::send_reschedule(target);
        }
    }
}

/// Выполняет переключение и роняет снятые процессы уже вне scheduler-лока:
/// `Drop` их capability будит припаркованные потоки через тот же лок.
pub(super) fn perform_schedule_action<A: ArchContext, T: TimerSource>(
    inner: &MutexCell<SchedulerInner<A, T>>,
    action: ScheduleAction<A>,
) {
    // До переключения: биты собраны под локом, а вход в поток, стартующий
    // впервые, не возвращается и хвост функции не исполняет.
    notify_pending_cpus(inner);

    if let ScheduleAction::Switch {
        prev,
        next,
        address_space,
    } = action
    {
        // Process-switch: активируем AS ДО context_switch, чтобы возобновляемый
        // user-thread проснулся уже в своём адресном пространстве.
        if let AddressSpaceTransition::Switch(handle) = address_space {
            A::switch_address_space(handle);
        }
        // SAFETY: raw pointers подготовлены под scheduler-lock-ом, а поток, на
        // который они смотрят, отмечен `previous` и до сюда не снимается.
        unsafe { A::switch(&mut *prev, &*next) };
    }

    // Ушедший в exit поток сюда не возвращается: отметку снимает и процесс
    // роняет тот поток, который следующим пройдёт через переключение.
    let dropped = inner.with_lock(SchedulerInner::finish_switch);
    drop(dropped);
}

unsafe extern "C" fn thread_trampoline<A: ArchContext>(arg: *mut ()) -> ! {
    // SAFETY: `arg` создан из `Box::into_raw(Box::new(TrampolinePayload { .. }))` в `spawn`.
    let payload: Box<TrampolinePayload> = unsafe { Box::from_raw(arg.cast()) };
    let TrampolinePayload { entry } = *payload;
    <A::Cpu as ArchCpu>::enable_preemption();
    entry();
    capability::thread_exit(0)
}
