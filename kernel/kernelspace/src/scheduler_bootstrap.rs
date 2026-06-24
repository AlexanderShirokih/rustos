//! Хелперы для инициализации scheduler-а из platform-independent кода.

use alloc::sync::Arc;

use drivers_common::services::timer::{TickHandler, TimerService};
use kobject::{
    IpcError, LoadImageError, ProcessObject, SpawnError, StartProcessError, ThreadObject,
    UserImageInstall, UserStartSpec, UserThreadEntry,
};
use memory::{UserVmContext, frame_allocator::FrameAllocator};
use scheduler::{
    ArchContext, Bootstrapped, Scheduler, SchedulerConfig, SchedulerHandle, SchedulerService,
    TimerSource, Uninit,
};

use crate::{kernel_context::KernelContext, syscall_bridge};

/// Адаптер `TimerService` (capability) -> `TimerSource`.
pub struct KernelTimerSource(Arc<dyn TimerService>);

impl KernelTimerSource {
    pub fn new(timer: Arc<dyn TimerService>) -> Self {
        Self(timer)
    }
}

impl TimerSource for KernelTimerSource {
    fn now_ns(&self) -> u64 {
        self.0.now_ns()
    }

    fn schedule_next(&self, deadline_ns: u64) {
        self.0.schedule_next(deadline_ns);
    }
}

struct SchedulerTickHandler<A, T>
where
    A: ArchContext,
    T: TimerSource,
{
    handle: SchedulerHandle<A, T>,
}

impl<A, T> TickHandler for SchedulerTickHandler<A, T>
where
    A: ArchContext,
    T: TimerSource,
{
    fn on_tick(&self, now_ns: u64) {
        self.handle.on_timer_tick(now_ns);
    }
}

struct SchedulerSyscallRuntime<A, T>
where
    A: ArchContext,
    T: TimerSource,
{
    handle: SchedulerHandle<A, T>,
}

impl<A, T> syscall_kernel::SyscallRuntime for SchedulerSyscallRuntime<A, T>
where
    A: ArchContext,
    T: TimerSource,
{
    fn current_user_vm(&self) -> Option<UserVmContext> {
        self.handle.current_user_vm()
    }

    fn current_ipc_buffer_va(&self) -> Option<u64> {
        self.handle
            .current_ipc_buffer_va()
            .map(|va| va.as_usize() as u64)
    }

    fn frame_allocator(&self) -> Option<&'static (dyn FrameAllocator + Send + Sync)> {
        syscall_bridge::frame_allocator()
    }

    fn current_thread_object(&self) -> Option<Arc<ThreadObject>> {
        self.handle.current_thread_object()
    }

    fn current_process_object(&self) -> Option<Arc<ProcessObject>> {
        self.handle.current_process_object()
    }

    fn create_empty_process(&self, name: &str) -> Result<Arc<ProcessObject>, SpawnError> {
        self.handle.create_empty_process(name)
    }

    fn create_user_thread(
        &self,
        process: &Arc<ProcessObject>,
        entry: UserThreadEntry,
    ) -> Result<Arc<ThreadObject>, SpawnError> {
        self.handle.create_user_thread(process, entry)
    }

    fn terminate_thread(&self, thread: &Arc<ThreadObject>, exit_code: i32) -> Result<(), IpcError> {
        self.handle.terminate_thread(thread, exit_code)
    }

    fn terminate_process(
        &self,
        process: &Arc<ProcessObject>,
        exit_code: i32,
    ) -> Result<(), IpcError> {
        self.handle.terminate_process(process, exit_code)
    }

    fn load_user_image_into(
        &self,
        process: &Arc<ProcessObject>,
        install: &UserImageInstall,
    ) -> Result<(), LoadImageError> {
        self.handle.load_user_image_into(process, install)
    }

    fn start_user_process(
        &self,
        process: &Arc<ProcessObject>,
        spec: UserStartSpec,
    ) -> Result<Arc<ThreadObject>, StartProcessError> {
        self.handle.start_user_process(process, spec)
    }
}

/// Создаёт scheduler, делает bootstrap, регистрирует `SchedulerService` в
/// bootstrap services и привязывает `TickHandler` к `TimerService`.
///
/// Возвращает scheduler в состоянии [`Bootstrapped`] - вызывающий должен
/// зарегистрировать начальные потоки и перевести scheduler в [`super::Running`]
/// через [`Scheduler::start`].
pub fn bootstrap_scheduler<A>(
    kernel: &mut KernelContext,
    config: SchedulerConfig,
) -> Scheduler<A, KernelTimerSource, Bootstrapped>
where
    A: ArchContext,
{
    let timer = kernel.with_runtime_state(|services, _| {
        services
            .require_timer()
            .expect("TimerService must be available before scheduler startup")
    });

    let factory = Some(kernel.address_space_factory());

    let scheduler = Scheduler::<A, KernelTimerSource, Uninit>::with_address_space_factory(
        KernelTimerSource::new(timer.clone()),
        config,
        factory,
    )
    .bootstrap();

    if let Some(fa) = syscall_bridge::frame_allocator() {
        scheduler.set_frame_allocator(fa);
    }

    let handle = scheduler.handle();
    let service: Arc<dyn SchedulerService> = Arc::new(handle.clone());
    let tick_handler: Arc<dyn TickHandler> = Arc::new(SchedulerTickHandler {
        handle: handle.clone(),
    });
    let kobject_runtime: Arc<dyn kobject::KernelRuntime> = Arc::new(handle.clone());
    let syscall_runtime: Arc<dyn syscall_kernel::SyscallRuntime> =
        Arc::new(SchedulerSyscallRuntime { handle });

    kernel.with_runtime_state(|services, _| {
        services
            .set_scheduler(service.clone())
            .expect("SchedulerService registration must succeed");
    });
    timer.set_handler(tick_handler);
    kobject::install_runtime(kobject_runtime);
    syscall_kernel::install_runtime(syscall_runtime);
    syscall_bridge::install_scheduler(service);

    scheduler
}
