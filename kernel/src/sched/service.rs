use alloc::{boxed::Box, sync::Arc};

use collections::LockCell;
use drivers_common::services::{
    scheduler::{SchedulerService, SpawnConfig, SpawnError, ThreadId},
    timer::TickHandler,
};

use super::{
    arch::{ArchContext, ArchCpu, TimerSource},
    scheduler::{SchedulerInner, perform_schedule_action},
};

/// Капабилити-handle на scheduler. Регистрируется в `Capabilities` как
/// `Arc<dyn SchedulerService>` и одновременно используется как `TickHandler`
/// для системного таймера.
pub struct SchedulerHandle<A, T, const PRIO: usize, const N_CPUS: usize, const N_THREADS: usize>
where
    A: ArchContext,
    T: TimerSource,
{
    inner: Arc<collections::MutexCell<SchedulerInner<A, T, PRIO, N_CPUS, N_THREADS>>>,
}

impl<A, T, const PRIO: usize, const N_CPUS: usize, const N_THREADS: usize>
    SchedulerHandle<A, T, PRIO, N_CPUS, N_THREADS>
where
    A: ArchContext,
    T: TimerSource,
{
    pub(crate) fn new(
        inner: Arc<collections::MutexCell<SchedulerInner<A, T, PRIO, N_CPUS, N_THREADS>>>,
    ) -> Self {
        Self { inner }
    }

    fn self_arc(&self) -> Arc<dyn SchedulerService> {
        Arc::new(Self {
            inner: self.inner.clone(),
        })
    }
}

impl<A, T, const PRIO: usize, const N_CPUS: usize, const N_THREADS: usize> TickHandler
    for SchedulerHandle<A, T, PRIO, N_CPUS, N_THREADS>
where
    A: ArchContext,
    T: TimerSource,
{
    fn on_tick(&self, now_ns: u64) {
        let action = self.inner.with_lock(|inner| inner.on_tick(now_ns));
        perform_schedule_action::<A>(action);
    }
}

impl<A, T, const PRIO: usize, const N_CPUS: usize, const N_THREADS: usize> SchedulerService
    for SchedulerHandle<A, T, PRIO, N_CPUS, N_THREADS>
where
    A: ArchContext,
    T: TimerSource,
{
    fn spawn_boxed(
        &self,
        cfg: SpawnConfig,
        entry: Box<dyn FnOnce() + Send + 'static>,
    ) -> Result<ThreadId, SpawnError> {
        let exit_handle = self.self_arc();
        self.inner
            .with_lock(|inner| inner.spawn_boxed(cfg, entry, exit_handle))
    }

    fn yield_now(&self) {
        <A::Cpu as ArchCpu>::disable_preemption();
        let action = self.inner.with_lock(|inner| {
            let now_ns = inner.now_ns();
            inner.yield_now(now_ns)
        });
        perform_schedule_action::<A>(action);
        <A::Cpu as ArchCpu>::enable_preemption();
    }

    fn sleep_ns(&self, ns: u64) {
        <A::Cpu as ArchCpu>::disable_preemption();
        let action = self.inner.with_lock(|inner| {
            let now_ns = inner.now_ns();
            inner.sleep_current(ns, now_ns)
        });
        perform_schedule_action::<A>(action);
        <A::Cpu as ArchCpu>::enable_preemption();
    }

    fn current(&self) -> ThreadId {
        self.inner.with_lock(|inner| inner.current())
    }

    fn exit(&self) -> ! {
        <A::Cpu as ArchCpu>::disable_preemption();
        let action = self.inner.with_lock(|inner| {
            let now_ns = inner.now_ns();
            inner.exit_current(now_ns)
        });
        perform_schedule_action::<A>(action);
        // После switch_to_next текущий поток уже не должен возвращаться.
        // Если выполнение вернулось - это серьёзный bug в context-switch.
        unreachable!("terminated thread resumed after scheduler switch")
    }
}
