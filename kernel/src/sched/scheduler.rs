use alloc::{boxed::Box, sync::Arc, vec::Vec};
use core::{array, marker::PhantomData, num::NonZeroU32};

use collections::{LockCell, MutexCell};
use drivers_common::services::scheduler::{Priority, SpawnConfig, SpawnError, ThreadId};

use super::{
    address_space::AddressSpace,
    arch::{ArchContext, TimerSource},
    cpu::Cpu,
    process::{Process, ProcessId},
    thread::{Thread, ThreadState},
    thread_table::ThreadTable,
    wait_queue::{SleepEntry, SleepQueue},
};

const DEFAULT_TIME_SLICE_TICKS: u32 = 1;
const DEFAULT_QUANTUM_NS: u64 = 10_000_000;

pub(super) enum ScheduleAction<A: ArchContext> {
    None,
    Switch { prev: *mut A, next: *const A },
}

pub trait SchedulerStage {}

pub struct Uninit;
pub struct Bootstrapped;
pub struct Running;

impl SchedulerStage for Uninit {}
impl SchedulerStage for Bootstrapped {}
impl SchedulerStage for Running {}

pub struct Scheduler<A, T, S, const PRIO: usize, const N_CPUS: usize, const N_THREADS: usize>
where
    A: ArchContext,
    T: TimerSource,
    S: SchedulerStage,
{
    pub(crate) inner: Arc<MutexCell<SchedulerInner<A, T, PRIO, N_CPUS, N_THREADS>>>,
    _stage: PhantomData<S>,
}

pub(crate) struct SchedulerInner<A, T, const PRIO: usize, const N_CPUS: usize, const N_THREADS: usize>
where
    A: ArchContext,
    T: TimerSource,
{
    timer: T,
    threads: ThreadTable<A, N_THREADS>,
    processes: Vec<Process>,
    next_process_id: u32,
    cpus: [Option<Cpu<A, PRIO>>; N_CPUS],
    sleepers: SleepQueue,
    kernel_address_space: Arc<AddressSpace>,
    quantum_ns: u64,
    time_slice_ticks: u32,
}

impl<A, T, const PRIO: usize, const N_CPUS: usize, const N_THREADS: usize>
    Scheduler<A, T, Uninit, PRIO, N_CPUS, N_THREADS>
where
    A: ArchContext,
    T: TimerSource,
{
    pub fn new(timer: T) -> Self {
        Self {
            inner: Arc::new(MutexCell::new(SchedulerInner {
                timer,
                threads: ThreadTable::new(),
                processes: Vec::new(),
                next_process_id: 1,
                cpus: array::from_fn(|_| None),
                sleepers: SleepQueue::new(),
                kernel_address_space: AddressSpace::shared_kernel(),
                quantum_ns: DEFAULT_QUANTUM_NS,
                time_slice_ticks: DEFAULT_TIME_SLICE_TICKS,
            })),
            _stage: PhantomData,
        }
    }

    pub fn bootstrap(self) -> Scheduler<A, T, Bootstrapped, PRIO, N_CPUS, N_THREADS> {
        let bootstrapped = Scheduler {
            inner: self.inner,
            _stage: PhantomData::<Bootstrapped>,
        };

        bootstrapped
            .inner
            .with_lock(|inner| inner.bootstrap_current_cpu());

        bootstrapped
    }
}

impl<A, T, const PRIO: usize, const N_CPUS: usize, const N_THREADS: usize>
    Scheduler<A, T, Bootstrapped, PRIO, N_CPUS, N_THREADS>
where
    A: ArchContext,
    T: TimerSource,
{
    pub fn handle(
        &self,
    ) -> super::service::SchedulerHandle<A, T, PRIO, N_CPUS, N_THREADS> {
        super::service::SchedulerHandle::new(self.inner.clone())
    }

    pub fn spawn<F>(&self, cfg: SpawnConfig, entry: F) -> Result<ThreadId, SpawnError>
    where
        F: FnOnce() + Send + 'static,
    {
        self.inner.with_lock(|inner| inner.spawn(cfg, entry))
    }

    pub fn run(self) -> Scheduler<A, T, Running, PRIO, N_CPUS, N_THREADS> {
        let running = Scheduler {
            inner: self.inner,
            _stage: PhantomData::<Running>,
        };

        <A::Cpu as super::arch::ArchCpu>::disable_preemption();
        let action = running.inner.with_lock(|inner| {
            let now_ns = inner.timer.now_ns();
            inner.switch_to_next(now_ns)
        });
        perform_schedule_action::<A>(action);
        <A::Cpu as super::arch::ArchCpu>::enable_preemption();

        running
    }

    pub fn start(self) -> ! {
        let next_ptr = self.inner.with_lock(|inner| inner.prepare_first_thread_start());

        // SAFETY: первый поток выбран под scheduler-lock, его контекст живёт в shared heap state.
        unsafe { A::start(&*next_ptr) }
    }
}

impl<A, T, const PRIO: usize, const N_CPUS: usize, const N_THREADS: usize>
    Scheduler<A, T, Running, PRIO, N_CPUS, N_THREADS>
where
    A: ArchContext,
    T: TimerSource,
{
    pub fn current(&self) -> ThreadId {
        self.inner.with_lock(|inner| inner.current())
    }

    pub fn yield_now(&self) {
        <A::Cpu as super::arch::ArchCpu>::disable_preemption();
        let action = self.inner.with_lock(|inner| {
            let now_ns = inner.timer.now_ns();
            inner.yield_now(now_ns)
        });
        perform_schedule_action::<A>(action);
        <A::Cpu as super::arch::ArchCpu>::enable_preemption();
    }

    pub fn sleep_ns(&self, ns: u64) {
        <A::Cpu as super::arch::ArchCpu>::disable_preemption();
        let action = self.inner.with_lock(|inner| {
            let now_ns = inner.timer.now_ns();
            inner.sleep_current(ns, now_ns)
        });
        perform_schedule_action::<A>(action);
        <A::Cpu as super::arch::ArchCpu>::enable_preemption();
    }

    pub fn on_tick(&self, now_ns: u64) {
        let action = self.inner.with_lock(|inner| inner.on_tick(now_ns));
        perform_schedule_action::<A>(action);
    }

    pub fn handle(
        &self,
    ) -> super::service::SchedulerHandle<A, T, PRIO, N_CPUS, N_THREADS> {
        super::service::SchedulerHandle::new(self.inner.clone())
    }
}

impl<A, T, const PRIO: usize, const N_CPUS: usize, const N_THREADS: usize>
    SchedulerInner<A, T, PRIO, N_CPUS, N_THREADS>
where
    A: ArchContext,
    T: TimerSource,
{
    pub(crate) fn spawn_boxed(
        &mut self,
        cfg: SpawnConfig,
        entry: Box<dyn FnOnce() + Send + 'static>,
    ) -> Result<ThreadId, SpawnError> {
        self.spawn(cfg, move || entry())
    }

    fn bootstrap_current_cpu(&mut self) {
        let cpu_id = <A::Cpu as super::arch::ArchCpu>::current_id();
        let idle_id = self
            .spawn(
                SpawnConfig::new("idle").priority(Priority::MAX),
                || <A::Cpu as super::arch::ArchCpu>::idle(),
            )
            .expect("idle thread bootstrap must succeed");

        let idle = self
            .threads
            .get_mut(idle_id)
            .expect("idle thread must exist after bootstrap");
        idle.set_state(ThreadState::Running);
        idle.set_time_slice_left(self.time_slice_ticks);

        let cpu = Cpu::new(cpu_id, idle_id);
        let slot = self
            .cpus
            .get_mut(cpu_id.as_index())
            .expect("current CPU index must fit configured CPU count");
        *slot = Some(cpu);

        // SAFETY: Cpu slot lives inside leaked/owned scheduler state for the kernel lifetime.
        unsafe {
            let cpu_ptr = slot
                .as_mut()
                .expect("CPU slot must be initialized")
                as *mut Cpu<A, PRIO>
                as *mut ();
            <A::Cpu as super::arch::ArchCpu>::install_cpu_local(cpu_ptr);
        }
    }

    fn spawn<F>(&mut self, cfg: SpawnConfig, entry: F) -> Result<ThreadId, SpawnError>
    where
        F: FnOnce() + Send + 'static,
    {
        if cfg.stack_pages == 0 {
            return Err(SpawnError::InvalidStackPages);
        }

        let stack =
            <A::Stack as super::arch::ArchStack>::allocate(cfg.stack_pages).map_err(|_| {
                SpawnError::StackAllocationFailed
            })?;
        let stack_top = stack.top();
        let entry: Box<dyn FnOnce() + Send + 'static> = Box::new(entry);
        let arg = Box::into_raw(Box::new(entry)).cast::<()>();
        let arch = A::init(stack_top, thread_trampoline::<A>, arg);
        let process = self.create_process(cfg.name);
        let thread = Thread::new(process.id(), cfg.priority, arch, stack, cfg.name);
        let id = self.threads.insert(thread)?;

        if let Some(cpu) = self.current_cpu_mut() {
            if cpu.idle() != id {
                cpu.ready_queue_mut().push(id, cfg.priority);
            }
        }

        Ok(id)
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
        if let Some(cpu) = self.current_cpu() {
            if cpu.ready_queue().is_empty() {
                self.schedule_next_deadline(now_ns);
                return ScheduleAction::None;
            }
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

    fn create_process(&mut self, name: &'static str) -> Process {
        let raw = NonZeroU32::new(self.next_process_id).expect("process id must never be zero");
        self.next_process_id = self.next_process_id.saturating_add(1);
        let process = Process::new(ProcessId::new(raw), name, self.kernel_address_space.clone());
        self.processes.push(Process::new(
            process.id(),
            process.name(),
            process.address_space().clone(),
        ));
        process
    }

    fn wake_sleepers(&mut self, now_ns: u64) {
        while self
            .sleepers
            .peek()
            .is_some_and(|entry| entry.wakeup_at_ns <= now_ns)
        {
            let entry = self.sleepers.pop().expect("peek returned entry");
            if let Some(thread) = self.threads.get_mut(entry.thread_id) {
                let priority = thread.priority();
                if matches!(thread.state(), ThreadState::Sleeping { .. }) {
                    thread.set_state(ThreadState::Ready);
                    thread.set_time_slice_left(self.time_slice_ticks);
                    let _ = thread;
                    if let Some(cpu) = self.current_cpu_mut() {
                        cpu.ready_queue_mut().push(entry.thread_id, priority);
                    }
                }
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

            if let Some(priority) = priority {
                if let Some(cpu) = self.current_cpu_mut() {
                    cpu.ready_queue_mut().push(current_id, priority);
                }
            }
        }

        self.switch_to_next(now_ns)
    }

    fn switch_to_next(&mut self, now_ns: u64) -> ScheduleAction<A> {
        let Some(cpu) = self.current_cpu_mut() else {
            return ScheduleAction::None;
        };

        let prev_id = cpu.current();
        let next_id = cpu
            .ready_queue_mut()
            .pop_highest()
            .map(|(id, _)| id)
            .unwrap_or_else(|| cpu.idle());

        cpu.set_current(next_id);

        if let Some(next) = self.threads.get_mut(next_id) {
            next.set_state(ThreadState::Running);
            next.set_time_slice_left(self.time_slice_ticks);
        }

        if prev_id == next_id {
            self.schedule_next_deadline(now_ns);
            return ScheduleAction::None;
        }

        let Some((prev, next)) = self.threads.split_pair_mut(prev_id, next_id) else {
            self.schedule_next_deadline(now_ns);
            return ScheduleAction::None;
        };

        if matches!(prev.state(), ThreadState::Running) {
            prev.set_state(ThreadState::Ready);
        }

        let action = {
            let prev_ptr = prev.arch_mut() as *mut A;
            let next_ptr = next.arch() as *const A;
            ScheduleAction::Switch {
                prev: prev_ptr,
                next: next_ptr,
            }
        };
        self.schedule_next_deadline(now_ns);
        action
    }

    fn prepare_first_thread_start(&mut self) -> *const A {
        let now_ns = self.timer.now_ns();
        let cpu = self.current_cpu_mut().expect("scheduler must be bootstrapped");
        let next_id = cpu
            .ready_queue_mut()
            .pop_highest()
            .map(|(id, _)| id)
            .unwrap_or_else(|| cpu.idle());

        cpu.set_current(next_id);

        let next_ptr = {
            let next = self
                .threads
                .get_mut(next_id)
                .expect("first runnable thread must exist");
            next.set_state(ThreadState::Running);
            next.set_time_slice_left(self.time_slice_ticks);
            next.arch() as *const A
        };
        self.schedule_next_deadline(now_ns);
        next_ptr
    }

    fn schedule_next_deadline(&self, now_ns: u64) {
        let wakeup_deadline = self.sleepers.peek().map(|entry| entry.wakeup_at_ns);
        let quantum_deadline = now_ns.saturating_add(self.quantum_ns);
        let deadline = wakeup_deadline.map_or(quantum_deadline, |wakeup_deadline| {
            wakeup_deadline.min(quantum_deadline)
        });
        self.timer.schedule_next(deadline);
    }

    fn current_cpu(&self) -> Option<&Cpu<A, PRIO>> {
        let cpu_id = <A::Cpu as super::arch::ArchCpu>::current_id();
        self.cpus.get(cpu_id.as_index())?.as_ref()
    }

    fn current_cpu_mut(&mut self) -> Option<&mut Cpu<A, PRIO>> {
        let cpu_id = <A::Cpu as super::arch::ArchCpu>::current_id();
        self.cpus.get_mut(cpu_id.as_index())?.as_mut()
    }
}

pub(super) fn perform_schedule_action<A: ArchContext>(action: ScheduleAction<A>) {
    if let ScheduleAction::Switch { prev, next } = action {
        // SAFETY: raw pointers are prepared under exclusive scheduler access and remain valid.
        unsafe { A::switch(&mut *prev, &*next) };
    }
}

unsafe extern "C" fn thread_trampoline<A: ArchContext>(arg: *mut ()) -> ! {
    // SAFETY: `arg` is created from `Box<Box<dyn FnOnce() + Send>>` in `spawn`.
    let entry: Box<Box<dyn FnOnce() + Send + 'static>> = unsafe { Box::from_raw(arg.cast()) };
    let entry = *entry;
    <A::Cpu as super::arch::ArchCpu>::enable_preemption();
    entry();
    panic!("thread returned without exit handler");
}
