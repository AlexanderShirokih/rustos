//! Production-init: запускает rootkeeper-цепочку userland.
//!
//! Передаётся в [`kmain`](crate::kmain::kmain) как init-таск; вызывается
//! после bootstrap'а scheduler-а и до перехода в `Scheduler::start`.

extern crate alloc;

use alloc::sync::Arc;

use klog::{info, warn};
use scheduler::{ArchContext, Bootstrapped, Priority, Scheduler, SchedulerService, SpawnConfig};

use crate::{
    bootstrap::{spawn_bootstrap_log, spawn_rootkeeper},
    kernel_context::KernelContext,
    scheduler_bootstrap::KernelTimerSource,
    user_process::{SchedulerUserProcessLauncher, UserProcessLauncher},
};

/// Спавнит init-процесс с приоритетом `highest`. Init-процесс запускает
/// rootkeeper-цепочку userland.
pub fn spawn_init_process<A>(
    scheduler: &Scheduler<A, KernelTimerSource, Bootstrapped>,
    kernel: &mut KernelContext,
) where
    A: ArchContext,
{
    // До спавна init-таска собираются только Copy/Send-значения; вся работа
    // с blob выполняется внутри уже работающего init-таска.
    let launcher: Arc<dyn UserProcessLauncher> = Arc::new(SchedulerUserProcessLauncher::new(
        scheduler.handle(),
        kernel.address_space_factory(),
    ));

    let blob = kernel.userland_blob();
    let user_va_end = A::USER_VA_END;

    let scheduler_service = kernel.with_runtime_state(|services, _| {
        services
            .require_scheduler()
            .expect("SchedulerService must be registered before init task spawn")
    });

    scheduler
        .spawn(
            SpawnConfig::new("init").priority(Priority::highest()),
            move || {
                start_rootkeeper_chain(launcher.as_ref(), blob, user_va_end, &scheduler_service);
            },
        )
        .expect("init process spawn must succeed");
}

/// Запускает rootkeeper и логгер его bootstrap-канала. Любая ошибка
/// userland-пути логируется warn'ом, ядро продолжает работу.
fn start_rootkeeper_chain(
    launcher: &dyn UserProcessLauncher,
    blob: Option<&'static [u8]>,
    user_va_end: usize,
    scheduler_service: &Arc<dyn SchedulerService>,
) {
    let Some(blob) = blob else {
        warn!("userland blob missing; rootkeeper not started");
        return;
    };

    let launch = match spawn_rootkeeper(launcher, blob, user_va_end) {
        Ok(launch) => launch,
        Err(e) => {
            warn!("rootkeeper spawn failed: {:?}", e);
            return;
        }
    };
    info!("rootkeeper spawned: {:?}", launch.info);

    if let Err(e) = spawn_bootstrap_log(scheduler_service, launch.channel) {
        warn!("bootstrap-log spawn failed: {}", e);
    }
}
