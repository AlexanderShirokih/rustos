use alloc::{boxed::Box, sync::Arc};

use collections::LockCell;
use drivers_common::services::{
    scheduler::{SchedulerService, SpawnConfig, SpawnError, ThreadId},
    timer::TickHandler,
};

use super::{
    arch::{ArchContext, ArchCpu, TimerSource, with_preemption_disabled},
    scheduler::{SchedulerInner, perform_schedule_action},
};

/// Капабилити-handle на scheduler. Регистрируется в `Capabilities` как
/// `Arc<dyn SchedulerService>` и одновременно используется как `TickHandler`
/// для системного таймера.
pub struct SchedulerHandle<A, T>
where
    A: ArchContext,
    T: TimerSource,
{
    inner: Arc<collections::MutexCell<SchedulerInner<A, T>>>,
}

impl<A, T> SchedulerHandle<A, T>
where
    A: ArchContext,
    T: TimerSource,
{
    pub(crate) fn new(
        inner: Arc<collections::MutexCell<SchedulerInner<A, T>>>,
    ) -> Self {
        Self { inner }
    }

    fn self_arc(&self) -> Arc<dyn SchedulerService> {
        Arc::new(Self {
            inner: self.inner.clone(),
        })
    }
}

impl<A, T> TickHandler for SchedulerHandle<A, T>
where
    A: ArchContext,
    T: TimerSource,
{
    fn on_tick(&self, now_ns: u64) {
        let action = self.inner.with_lock(|inner| inner.on_tick(now_ns));
        perform_schedule_action::<A>(action);
    }
}

impl<A, T> SchedulerService for SchedulerHandle<A, T>
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
        let action = with_preemption_disabled::<A::Cpu, _>(|| {
            self.inner.with_lock(|inner| {
            let now_ns = inner.now_ns();
            inner.yield_now(now_ns)
            })
        });
        perform_schedule_action::<A>(action);
    }

    fn sleep_ns(&self, ns: u64) {
        let action = with_preemption_disabled::<A::Cpu, _>(|| {
            self.inner.with_lock(|inner| {
            let now_ns = inner.now_ns();
            inner.sleep_current(ns, now_ns)
            })
        });
        perform_schedule_action::<A>(action);
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
        // После switch_to_next текущий поток не должен возвращаться.
        // Если выполнение вернулось - это серьёзный bug в context-switch.
        unreachable!("terminated thread resumed after scheduler switch")
    }
}
