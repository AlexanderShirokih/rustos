use alloc::{boxed::Box, sync::Arc};
use core::sync::atomic::{AtomicU32, Ordering};

use collections::{LockCell, MutexCell};
use kobject::{
    HandleTable, IpcError, KernelRuntime, LoadImageError, ParkState, ProcessObject,
    StartProcessError, ThreadObject, UserImageInstall, UserStartSpec, UserThreadEntry, WaitToken,
};
use memory::{UserVmContext, virtual_address::VirtualAddress};

use super::{
    arch::{ArchContext, ArchCpu, TimerSource, with_preemption_disabled},
    scheduler::{ScheduleAction, SchedulerInner, perform_schedule_action},
};
use crate::{
    PreparedUserProcess, PreparedUserProcessError, SchedulerService, SpawnConfig, SpawnError,
    ThreadId,
};

/// Thread-safe handle на планировщик для использования вне scheduler-lock.
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

    /// User-VA IPC-буфера текущего потока; `None` для kernel-потоков или до bootstrap.
    pub fn current_ipc_buffer_va(&self) -> Option<VirtualAddress> {
        self.inner.with_lock(|inner| inner.current_ipc_buffer_va())
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
        self.inner.with_lock(|inner| inner.spawn_boxed(cfg, entry))
    }

    fn yield_now(&self) {
        with_preemption_disabled::<A::Cpu, _>(|| {
            let action = self.inner.with_lock(|inner| {
                let now_ns = inner.now_ns();
                inner.yield_now(now_ns)
            });
            perform_schedule_action::<A>(action);
        });
    }

    fn sleep_ns(&self, ns: u64) {
        with_preemption_disabled::<A::Cpu, _>(|| {
            let action = self.inner.with_lock(|inner| {
                let now_ns = inner.now_ns();
                inner.sleep_current(ns, now_ns)
            });
            perform_schedule_action::<A>(action);
        });
    }

    fn current(&self) -> ThreadId {
        self.inner.with_lock(|inner| inner.current())
    }

    fn exit(&self) -> ! {
        self.exit_current_thread(0)
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

    fn current_thread_object(&self) -> Option<Arc<ThreadObject>> {
        self.inner.with_lock(|inner| inner.current_thread_object())
    }

    fn current_process_object(&self) -> Option<Arc<ProcessObject>> {
        self.inner.with_lock(|inner| inner.current_process_object())
    }

    fn exit_current_thread(&self, exit_code: i32) -> ! {
        // Bare disable: путь не возвращается, парный enable не нужен.
        <A::Cpu as ArchCpu>::disable_preemption();
        let signals = self
            .inner
            .with_lock(|inner| inner.begin_exit_current(exit_code));
        signals.emit();
        let action = self.inner.with_lock(|inner| {
            let now_ns = inner.now_ns();
            inner.finish_exit_current(now_ns)
        });
        perform_schedule_action::<A>(action);
        unreachable!("terminated thread resumed after scheduler switch")
    }

    fn block_current_until(&self, ready_flag: &AtomicU32, timeout_ns: Option<u64>) {
        with_preemption_disabled::<A::Cpu, _>(|| {
            let action = self.inner.with_lock(|inner| {
                // Если waker успел отработать до того, как мы взяли
                // scheduler-lock, не уходим в блокировку - иначе никто
                // не разбудит нас обратно.
                if ready_flag.load(Ordering::Acquire) != ParkState::REGISTERED {
                    return ScheduleAction::None;
                }
                let now_ns = inner.now_ns();
                inner.block_current(now_ns, timeout_ns)
            });
            perform_schedule_action::<A>(action);
        });
    }

    fn unblock(&self, token: WaitToken) {
        let Ok(thread_id) = ThreadId::try_from(token) else {
            return;
        };
        self.inner
            .with_lock(|inner| inner.unblock_thread(thread_id));
    }

    fn create_empty_process(&self, name: &str) -> Result<Arc<ProcessObject>, kobject::SpawnError> {
        self.inner
            .with_lock(|inner| inner.create_empty_process(name))
            .map_err(Into::into)
    }

    fn create_user_thread(
        &self,
        process: &Arc<ProcessObject>,
        entry: UserThreadEntry,
    ) -> Result<Arc<ThreadObject>, kobject::SpawnError> {
        self.inner
            .with_lock(|inner| inner.create_user_thread(process, entry))
            .map_err(Into::into)
    }

    fn terminate_thread(&self, thread: &Arc<ThreadObject>, exit_code: i32) -> Result<(), IpcError> {
        let signals = self
            .inner
            .with_lock(|inner| inner.terminate_thread_ko(thread, exit_code));
        signals.emit();
        Ok(())
    }

    fn terminate_process(
        &self,
        process: &Arc<ProcessObject>,
        exit_code: i32,
    ) -> Result<(), IpcError> {
        let signals = self
            .inner
            .with_lock(|inner| inner.terminate_process_ko(process, exit_code));
        signals.emit();
        Ok(())
    }

    fn load_user_image_into(
        &self,
        process: &Arc<ProcessObject>,
        install: &UserImageInstall,
    ) -> Result<(), LoadImageError> {
        self.inner
            .with_lock(|inner| inner.load_user_image_into(process, install))
    }

    fn start_user_process(
        &self,
        process: &Arc<ProcessObject>,
        spec: UserStartSpec,
    ) -> Result<Arc<ThreadObject>, StartProcessError> {
        self.inner
            .with_lock(|inner| inner.start_user_process(process, spec))
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
