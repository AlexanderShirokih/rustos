//! Helpers для инициализации scheduler-а из ARCH-крейтов.
//!
//! Изолирует логику создания scheduler, регистрации `SchedulerService` в
//! `Capabilities` и привязки к `TimerService`, чтобы ARCH-слой не дублировал её.

use alloc::sync::Arc;

use drivers_common::{
    CapabilityStoreExt, CapabilityStoreMutExt,
    services::{
        scheduler::SchedulerService,
        timer::{TickHandler, TimerService},
    },
};

use crate::kernel_context::KernelContext;

use super::{
    arch::{ArchContext, TimerSource},
    scheduler::{Bootstrapped, Scheduler, Uninit},
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
pub fn bootstrap_scheduler<A, const PRIO: usize, const N_CPUS: usize, const N_THREADS: usize>(
    kernel: &mut KernelContext,
) -> Scheduler<A, KernelTimerSource, Bootstrapped, PRIO, N_CPUS, N_THREADS>
where
    A: ArchContext,
{
    let timer = kernel.with_runtime_state(|caps, _| {
        caps.require_service::<dyn TimerService>()
            .expect("TimerService must be available before scheduler startup")
    });

    let scheduler = Scheduler::<A, KernelTimerSource, Uninit, PRIO, N_CPUS, N_THREADS>::new(
        KernelTimerSource::new(timer.clone()),
    )
    .bootstrap();

    let handle = Arc::new(scheduler.handle());
    let service: Arc<dyn SchedulerService> = handle.clone();
    let tick_handler: Arc<dyn TickHandler> = handle;

    kernel.with_runtime_state(|caps, _| {
        caps.provide_service::<dyn SchedulerService>(service)
            .expect("SchedulerService registration must succeed");
    });
    timer.set_handler(tick_handler);

    scheduler
}
