//! Production-init: спавнит первичный init-процесс и демо-задачи.
//!
//! Передаётся в [`kmain`](crate::kmain::kmain) как init-таск; вызывается
//! после bootstrap'а scheduler-а и до перехода в `Scheduler::start`.

extern crate alloc;

use alloc::sync::Arc;

use drivers_common::{
    CapabilityStoreExt,
    services::scheduler::{Priority, SchedulerService, SchedulerServiceExt, SpawnConfig},
};
use klog::info;

use crate::{
    kernel_context::KernelContext,
    sched::{ArchContext, Bootstrapped, KernelTimerSource, Scheduler},
};

/// Спавнит init-процесс с приоритетом `highest`. Init-процесс
/// разворачивает timer-server и демо-таски.
pub fn spawn_init_process<A>(
    scheduler: &Scheduler<A, KernelTimerSource, Bootstrapped>,
    kernel: &mut KernelContext,
) where
    A: ArchContext,
{
    let scheduler_service = kernel.with_runtime_state(|caps, _| {
        caps.require_service::<dyn SchedulerService>()
            .expect("SchedulerService must be registered before init task spawn")
    });

    scheduler
        .spawn(
            SpawnConfig::new("init").priority(Priority::highest()),
            move || spawn_demo_processes(&scheduler_service),
        )
        .expect("init process spawn must succeed");
}

fn spawn_demo_processes(scheduler_service: &Arc<dyn SchedulerService>) {
    use crate::timer_server::{pilot_client_subscribe, pilot_tick, spawn_timer_server};

    let client_end =
        spawn_timer_server(scheduler_service).expect("timer-server spawn must succeed");

    scheduler_service
        .spawn(
            SpawnConfig::new("test-process").priority(Priority::normal()),
            move || {
                let handles = match pilot_client_subscribe(&client_end) {
                    Ok(h) => h,
                    Err(e) => {
                        klog::warn!("pilot subscribe failed: {:?}", e);
                        return;
                    }
                };
                let period_ms = 200_u64;
                let mut tick = 0_u64;
                loop {
                    info!("Pilot tick #{tick} via channel-based Timer");
                    if let Err(e) = pilot_tick(&handles, period_ms) {
                        klog::warn!("pilot tick failed: {:?}", e);
                        return;
                    }
                    tick = tick.wrapping_add(1);
                }
            },
        )
        .expect("test-process spawn must succeed");

    for (idx, period_ms) in [(1_u32, 300_u64), (2, 700)] {
        let thread_service = scheduler_service.clone();
        scheduler_service
            .spawn(
                SpawnConfig::new("demo").priority(Priority::normal()),
                move || loop {
                    info!("Process {idx} tick");
                    thread_service.sleep_ms(period_ms);
                },
            )
            .expect("demo process spawn must succeed");
    }
}
