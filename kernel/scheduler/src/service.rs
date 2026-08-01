#![allow(unsafe_code)]

use alloc::{boxed::Box, sync::Arc};
use core::sync::atomic::{AtomicU32, Ordering};

use capability::{
    HandleTable, IpcError, KernelRuntime, LoadImageError, ParkState, ProcessObject,
    StartProcessError, ThreadObject, UserImageInstall, UserStartSpec, UserThreadEntry, WaitToken,
};
use collections::{LockCell, MutexCell};
use memory::{UserVmContext, virtual_address::VirtualAddress};

use super::{
    arch::{ArchContext, ArchCpu, TimerSource, with_preemption_disabled},
    scheduler::{
        CpuBootstrapError, DeferredSignals, ScheduleAction, SchedulerInner, notify_pending_cpus,
        perform_schedule_action, release_deferred_handles,
    },
};
use crate::{
    PreparedUserProcess, PreparedUserProcessError, SchedulerService, SpawnConfig, SpawnError,
    ThreadId,
};

/// Wait-token на случай отсутствия текущего потока. Значение вне диапазона `ThreadId`,
/// `unblock` по нему не находит поток.
const NO_CURRENT_THREAD_TOKEN: WaitToken = WaitToken::new(core::num::NonZeroU64::MAX);

/// Снимок IPC-контекста текущего потока: user-VM его процесса, VA IPC-буфера
/// и handle-таблица (см. [`SchedulerHandle::current_ipc_context`]).
pub struct CurrentIpcContext {
    pub user_vm: UserVmContext,
    pub ipc_buffer_va: VirtualAddress,
    pub handle_table: Arc<MutexCell<HandleTable>>,
}

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
    fn emit_deferred_signals(&self, signals: DeferredSignals) {
        let completed = signals.emit();
        self.inner
            .with_lock(|inner| inner.finish_deferred_cleanup(&completed));
    }

    pub(crate) fn new(inner: Arc<collections::MutexCell<SchedulerInner<A, T>>>) -> Self {
        Self { inner }
    }

    pub fn on_timer_tick(&self, now_ns: u64) {
        let action = self.inner.with_lock(|inner| inner.on_tick(now_ns));
        perform_schedule_action(&self.inner, action);
    }

    /// Перевыбор потока по требованию другого CPU: на этом CPU появилась
    /// работа, пока он мог стоять в idle.
    pub fn on_reschedule(&self) {
        with_preemption_disabled::<A::Cpu, _>(|| {
            let action = self.inner.with_lock(|inner| {
                let now_ns = inner.now_ns();
                inner.reschedule(now_ns)
            });
            perform_schedule_action(&self.inner, action);
        });
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

    /// Снимок IPC-контекста текущего потока за один захват scheduler-лока:
    /// user-VM, VA IPC-буфера и handle-таблица. `None` для kernel-потоков и до bootstrap.
    pub fn current_ipc_context(&self) -> Option<CurrentIpcContext> {
        let parts = self.inner.with_lock(|inner| inner.current_ipc_parts())?;
        let mapper = parts.address_space.mapper_arc()?;
        Some(CurrentIpcContext {
            user_vm: UserVmContext::new(mapper, parts.allocator),
            ipc_buffer_va: parts.ipc_buffer_va,
            handle_table: parts.handle_table,
        })
    }

    pub fn spawn_prepared_user_process(
        &self,
        prepared: PreparedUserProcess,
    ) -> Result<crate::UserProcessLaunchInfo, PreparedUserProcessError> {
        let result = self
            .inner
            .with_lock(|inner| inner.spawn_prepared_user_process(prepared));
        release_deferred_handles(&self.inner);
        result
    }

    pub fn current_thread_object(&self) -> Option<Arc<ThreadObject>> {
        self.inner.with_lock(|inner| inner.current_thread_object())
    }

    pub fn current_process_object(&self) -> Option<Arc<ProcessObject>> {
        self.inner.with_lock(|inner| inner.current_process_object())
    }

    pub fn create_empty_process(
        &self,
        name: &str,
    ) -> Result<Arc<ProcessObject>, capability::SpawnError> {
        self.inner
            .with_lock(|inner| inner.create_empty_process(name))
            .map_err(Into::into)
    }

    pub fn create_user_thread(
        &self,
        process: &Arc<ProcessObject>,
        entry: UserThreadEntry,
    ) -> Result<Arc<ThreadObject>, capability::SpawnError> {
        let result = self
            .inner
            .with_lock(|inner| inner.create_user_thread(process, entry))
            .map_err(Into::into);
        notify_pending_cpus(&self.inner);
        result
    }

    pub fn terminate_thread(
        &self,
        thread: &Arc<ThreadObject>,
        exit_code: i32,
    ) -> Result<(), IpcError> {
        let signals = self
            .inner
            .with_lock(|inner| inner.terminate_thread_object(thread, exit_code));
        self.emit_deferred_signals(signals);
        notify_pending_cpus(&self.inner);
        Ok(())
    }

    pub fn terminate_process(
        &self,
        process: &Arc<ProcessObject>,
        exit_code: i32,
    ) -> Result<(), IpcError> {
        let signals = self
            .inner
            .with_lock(|inner| inner.terminate_process_object(process, exit_code));
        self.emit_deferred_signals(signals);
        notify_pending_cpus(&self.inner);
        Ok(())
    }

    pub fn load_user_image_into(
        &self,
        process: &Arc<ProcessObject>,
        install: &UserImageInstall,
    ) -> Result<(), LoadImageError> {
        self.inner
            .with_lock(|inner| inner.load_user_image_into(process, install))
    }

    pub fn start_user_process(
        &self,
        process: &Arc<ProcessObject>,
        spec: UserStartSpec,
    ) -> Result<Arc<ThreadObject>, StartProcessError> {
        let result = self
            .inner
            .with_lock(|inner| inner.start_user_process(process, spec));
        release_deferred_handles(&self.inner);
        result
    }
}

/// Передача CPU планировщику.
///
/// Контракт отделён от [`SchedulerService`]: тот описывает работу с потоками и
/// доступен всем, а этот вызывается ровно один раз с загрузочного пути CPU.
pub trait CpuEntry: Send + Sync {
    /// Создаёт per-CPU состояние планировщика на текущем CPU: очередь готовых
    /// и idle-поток. Отделён от [`Self::enter`] тем, что возвращается: пока
    /// отказ ещё можно доложить, CPU не считается поднятым.
    ///
    /// Вызывается при замаскированных IRQ.
    fn bootstrap(&self) -> Result<(), CpuBootstrapError>;

    /// Отдаёт текущий CPU планировщику. Не возвращается: дальше CPU живёт под
    /// его управлением, а в отсутствие работы стоит в своём idle-потоке.
    ///
    /// Вход в поток размаскирует IRQ: к этому моменту per-CPU состояние на
    /// месте, и обработчику есть с чем работать.
    fn enter(&self) -> !;
}

impl<A, T> CpuEntry for SchedulerHandle<A, T>
where
    A: ArchContext,
    T: TimerSource,
{
    fn bootstrap(&self) -> Result<(), CpuBootstrapError> {
        <A::Cpu as ArchCpu>::disable_preemption();
        self.inner.with_lock(SchedulerInner::bootstrap_current_cpu)
    }

    fn enter(&self) -> ! {
        <A::Cpu as ArchCpu>::disable_preemption();

        let Some((next_ptr, address_space)) = self
            .inner
            .with_lock(SchedulerInner::prepare_first_thread_start)
        else {
            <A::Cpu as ArchCpu>::idle();
        };

        A::switch_address_space(address_space);
        // SAFETY: как в `Scheduler::start` - указатель на контекст выбранного
        // потока подготовлен под локом, IRQ замаскированы, и вход в поток
        // происходит ровно один раз на этом CPU.
        unsafe { A::start(&*next_ptr) }
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
        let spawned = self.inner.with_lock(|inner| inner.spawn_boxed(cfg, entry));
        notify_pending_cpus(&self.inner);
        spawned
    }

    fn yield_now(&self) {
        with_preemption_disabled::<A::Cpu, _>(|| {
            let action = self.inner.with_lock(|inner| {
                let now_ns = inner.now_ns();
                inner.yield_now(now_ns)
            });
            perform_schedule_action(&self.inner, action);
        });
    }

    fn sleep_ns(&self, ns: u64) {
        with_preemption_disabled::<A::Cpu, _>(|| {
            let action = self.inner.with_lock(|inner| {
                let now_ns = inner.now_ns();
                inner.sleep_current(ns, now_ns)
            });
            perform_schedule_action(&self.inner, action);
        });
    }

    fn current(&self) -> Option<ThreadId> {
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
        let Some(current) = self.inner.with_lock(|inner| inner.current()) else {
            debug_assert!(false, "wait token requested before scheduler bootstrap");
            return NO_CURRENT_THREAD_TOKEN;
        };
        WaitToken::from(current)
    }

    fn now_ns(&self) -> u64 {
        self.inner.with_lock(|inner| inner.now_ns())
    }

    fn current_handle_table(&self) -> Option<Arc<MutexCell<HandleTable>>> {
        self.inner.with_lock(|inner| inner.current_handle_table())
    }

    fn exit_current_thread(&self, exit_code: i32) -> ! {
        // Bare disable: путь не возвращается, парный enable не нужен.
        <A::Cpu as ArchCpu>::disable_preemption();
        let signals = self
            .inner
            .with_lock(|inner| inner.begin_exit_current(exit_code));
        let deferred = signals.emit();
        debug_assert!(deferred.is_empty());
        let action = self.inner.with_lock(|inner| {
            let now_ns = inner.now_ns();
            inner.finish_exit_current(now_ns)
        });
        perform_schedule_action(&self.inner, action);
        unreachable!("terminated thread resumed after scheduler switch")
    }

    fn block_current_until(&self, ready_flag: &AtomicU32, timeout_ns: Option<u64>) {
        with_preemption_disabled::<A::Cpu, _>(|| {
            let action = self.inner.with_lock(|inner| {
                // Если waker успел отработать до того, как мы взяли
                // scheduler-lock, не уходим в блокировку - иначе никто
                // не разбудит нас обратно. Terminated current обязан уйти с
                // CPU независимо от состояния waker.
                if !inner.current_is_terminated()
                    && ready_flag.load(Ordering::Acquire) != ParkState::REGISTERED
                {
                    return ScheduleAction::None;
                }
                let now_ns = inner.now_ns();
                inner.block_current(now_ns, timeout_ns)
            });
            perform_schedule_action(&self.inner, action);
        });
    }

    fn unblock(&self, token: WaitToken) {
        let Ok(thread_id) = ThreadId::try_from(token) else {
            return;
        };
        self.inner
            .with_lock(|inner| inner.unblock_thread(thread_id));
        notify_pending_cpus(&self.inner);
    }

    fn set_blocked_cancel(&self, cancel: Arc<dyn capability::CancelTarget>) {
        let rejected = self
            .inner
            .with_lock(|inner| inner.set_current_blocked_cancel(cancel));
        if let Some((thread_id, cancel)) = rejected {
            cancel.cancel();
            if let Some(reply) = cancel.take_abandoned_reply() {
                reply.cancel();
            }
            if let Some(thread_id) = thread_id {
                self.inner
                    .with_lock(|inner| inner.finish_deferred_cleanup(&[thread_id]));
            }
        }
    }

    fn clear_blocked_cancel(&self) {
        self.inner
            .with_lock(SchedulerInner::clear_current_blocked_cancel);
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
