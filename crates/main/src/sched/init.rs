//! Helpers для инициализации scheduler-а из platform-independent кода.
//!
//! Изолирует логику создания scheduler, регистрации `SchedulerService` в
//! `Capabilities` и привязки к `TimerService`.

use alloc::sync::Arc;

use drivers_common::{
    CapabilityStoreExt, CapabilityStoreMutExt,
    services::{
        scheduler::SchedulerService,
        timer::{TickHandler, TimerService},
    },
};

use super::{
    arch::{ArchContext, TimerSource},
    scheduler::{Bootstrapped, Scheduler, SchedulerConfig, Uninit},
};
use crate::{
    kernel_context::KernelContext,
    kobject::{KernelRuntime, install_runtime},
};

/// Адаптер `TimerService` (capability) -> `TimerSource` (требование scheduler).
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

/// Создаёт scheduler, делает bootstrap, регистрирует `SchedulerService` в
/// `Capabilities` и привязывает `TickHandler` к `TimerService`.
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
    let timer = kernel.with_runtime_state(|caps, _| {
        caps.require_service::<dyn TimerService>()
            .expect("TimerService must be available before scheduler startup")
    });

    let scheduler = Scheduler::<A, KernelTimerSource, Uninit>::new(
        KernelTimerSource::new(timer.clone()),
        config,
    )
    .bootstrap();

    let handle = Arc::new(scheduler.handle());
    let service: Arc<dyn SchedulerService> = handle.clone();
    let tick_handler: Arc<dyn TickHandler> = handle.clone();
    let runtime: Arc<dyn KernelRuntime> = handle;

    kernel.with_runtime_state(|caps, _| {
        caps.provide_service::<dyn SchedulerService>(service)
            .expect("SchedulerService registration must succeed");
    });
    timer.set_handler(tick_handler);
    install_runtime(runtime);

    scheduler
}
