use alloc::{boxed::Box, sync::Arc};
use core::sync::atomic::{AtomicU32, Ordering};

use collections::{LockCell, MutexCell};
use kobject::{HandleTable, KernelRuntime, ParkState, WaitToken};
use memory::UserVmContext;

use super::{
    arch::{ArchContext, ArchCpu, TimerSource, with_preemption_disabled},
    scheduler::{ScheduleAction, SchedulerInner, perform_schedule_action},
};
use crate::{
    PreparedUserProcess, PreparedUserProcessError, SchedulerService, SpawnConfig, SpawnError,
    ThreadId,
};

/// Thread-safe handle на scheduler.
pub struct SchedulerHandle<A, T>
where
    A: ArchContext,
    T: TimerSource,
{
    inner: Arc<collections::MutexCell<SchedulerInner<A, T>>>,
}

impl<A, T> Clone for SchedulerHandle<A, T>
where
    A: ArchContext,
    T: TimerSource,
{
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
        }
    }
}

impl<A, T> SchedulerHandle<A, T>
where
    A: ArchContext,
    T: TimerSource,
{
    pub(crate) fn new(inner: Arc<collections::MutexCell<SchedulerInner<A, T>>>) -> Self {
        Self { inner }
    }

    fn self_arc(&self) -> Arc<dyn SchedulerService> {
        Arc::new(Self {
            inner: self.inner.clone(),
        })
    }

    pub fn on_timer_tick(&self, now_ns: u64) {
        let action = self.inner.with_lock(|inner| inner.on_tick(now_ns));
        perform_schedule_action::<A>(action);
    }

    pub fn current_user_vm(&self) -> Option<UserVmContext> {
        let (address_space, allocator) =
            self.inner.with_lock(|inner| inner.current_user_vm_pair())?;
        let mapper = address_space.mapper_arc()?;
        Some(UserVmContext::new(mapper, allocator))
    }

    pub fn spawn_prepared_user_process(
        &self,
        prepared: PreparedUserProcess,
    ) -> Result<crate::UserProcessLaunchInfo, PreparedUserProcessError> {
        self.inner
            .with_lock(|inner| inner.spawn_prepared_user_process(prepared))
    }
}

pub trait UserProcessLauncher: Send + Sync {
    fn spawn_prepared_user_process(
        &self,
        prepared: PreparedUserProcess,
    ) -> Result<crate::UserProcessLaunchInfo, PreparedUserProcessError>;
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

impl<A, T> KernelRuntime for SchedulerHandle<A, T>
where
    A: ArchContext,
    T: TimerSource,
{
    fn current_wait_token(&self) -> WaitToken {
        self.inner.with_lock(|inner| inner.current()).into()
    }

    fn current_handle_table(&self) -> Option<Arc<MutexCell<HandleTable>>> {
        self.inner.with_lock(|inner| inner.current_handle_table())
    }

    fn block_current_until(&self, ready_flag: &AtomicU32, timeout_ns: Option<u64>) {
        let action = with_preemption_disabled::<A::Cpu, _>(|| {
            self.inner.with_lock(|inner| {
                // Если waker успел отработать до того, как мы взяли
                // scheduler-lock, не уходим в блокировку - иначе никто
                // не разбудит нас обратно.
                if ready_flag.load(Ordering::Acquire) != ParkState::REGISTERED {
                    return ScheduleAction::None;
                }
                let now_ns = inner.now_ns();
                inner.block_current(now_ns, timeout_ns)
            })
        });
        perform_schedule_action::<A>(action);
    }

    fn unblock(&self, token: WaitToken) {
        let Ok(thread_id) = ThreadId::try_from(token) else {
            return;
        };
        self.inner
            .with_lock(|inner| inner.unblock_thread(thread_id));
    }
}

impl<A, T> UserProcessLauncher for SchedulerHandle<A, T>
where
    A: ArchContext,
    T: TimerSource,
{
    fn spawn_prepared_user_process(
        &self,
        prepared: PreparedUserProcess,
    ) -> Result<crate::UserProcessLaunchInfo, PreparedUserProcessError> {
        self.spawn_prepared_user_process(prepared)
    }
}
