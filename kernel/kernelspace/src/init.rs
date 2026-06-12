//! Production-init: запускает rootkeeper-цепочку userland.
//!
//! Передаётся в [`kmain`](crate::kmain::kmain) как init-таск; завершение
//! rootkeeper-процесса выключает машину с его exit code.

extern crate alloc;

use alloc::sync::Arc;

use klog::{info, warn};
use kobject::{Handle, KObject, PROCESS_TERMINATED, Rights, install_handle, object_wait_one};
use scheduler::{ArchContext, Bootstrapped, Priority, Scheduler, SpawnConfig};

use crate::{
    bootstrap::{run_bootstrap_log, spawn_rootkeeper},
    kernel_context::KernelContext,
    power,
    scheduler_bootstrap::KernelTimerSource,
    user_process::{SchedulerUserProcessLauncher, UserProcessLauncher},
};

/// Спавнит init-процесс с приоритетом `highest`. Init-процесс ведёт
/// rootkeeper-цепочку userland и выключает машину по её завершении.
pub fn spawn_init_process<A>(
    scheduler: &Scheduler<A, KernelTimerSource, Bootstrapped>,
    kernel: &mut KernelContext,
) where
    A: ArchContext,
{
    let launcher: Arc<dyn UserProcessLauncher> = Arc::new(SchedulerUserProcessLauncher::new(
        scheduler.handle(),
        kernel.address_space_factory(),
    ));

    let blob = kernel.userland_blob();
    let user_va_end = A::USER_VA_END;

    scheduler
        .spawn(
            SpawnConfig::new("init").priority(Priority::highest()),
            move || start_rootkeeper_chain(launcher.as_ref(), blob, user_va_end),
        )
        .expect("init process spawn must succeed");
}

/// Запускает rootkeeper, дренирует его bootstrap-канал и по завершении
/// процесса выключает машину с его exit code; ошибка пути = выключение с кодом 1.
fn start_rootkeeper_chain(
    launcher: &dyn UserProcessLauncher,
    blob: Option<&'static [u8]>,
    user_va_end: usize,
) -> ! {
    let Some(blob) = blob else {
        warn!("userland blob missing; rootkeeper not started");
        power::system_off(1)
    };

    let launch = match spawn_rootkeeper(launcher, blob, user_va_end) {
        Ok(launch) => launch,
        Err(e) => {
            warn!("rootkeeper spawn failed: {:?}", e);
            power::system_off(1)
        }
    };
    info!("rootkeeper spawned: {:?}", launch.info);

    run_bootstrap_log(&launch.channel);

    let process_object = launch.info.process_object;
    let process_handle = match install_handle(Handle::new(
        KObject::Process(process_object.clone()),
        Rights::WAIT | Rights::INSPECT,
    )) {
        Ok(id) => id,
        Err(e) => {
            warn!("init: install_handle failed: {:?}", e);
            power::system_off(1)
        }
    };
    if let Err(e) = object_wait_one(process_handle, PROCESS_TERMINATED, None) {
        warn!("init: wait for rootkeeper exit failed: {:?}", e);
        power::system_off(1)
    }

    let exit_code = process_object.exit_code();
    info!("rootkeeper exited with code {}", exit_code);
    power::system_off(exit_code)
}
