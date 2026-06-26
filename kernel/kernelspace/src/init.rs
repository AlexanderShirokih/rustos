//! Production-init: запускает bootstrap-цепочку userland.
//!
//! Передаётся в [`kmain`](crate::kmain::kmain) как init-таск; завершение
//! bootstrap-процесса выключает машину с его exit code.

extern crate alloc;

use alloc::sync::Arc;

use capability::{Capability, CapabilityTarget, Rights, SIGNALED, install_handle, signal_wait_one};
use klog::{info, warn};
use scheduler::{ArchContext, Bootstrapped, Priority, Scheduler, SchedulerServiceExt, SpawnConfig};

use crate::{
    bootstrap::{run_bootstrap, spawn_process},
    kernel_context::{KernelContext, UserlandImage},
    power,
    scheduler_bootstrap::KernelTimerSource,
    syscall_bridge,
    user_process::{SchedulerUserProcessLauncher, UserProcessLauncher},
};

/// Спавнит init-процесс с приоритетом `highest`. Init-процесс ведёт
/// bootstrap-цепочку userland и выключает машину по её завершении.
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

    let image = kernel.userland_image();
    let user_va_end = A::USER_VA_END;

    scheduler
        .spawn(
            SpawnConfig::new("init").priority(Priority::highest()),
            move || start_bootstrap_chain(launcher.as_ref(), image, user_va_end),
        )
        .expect("init process spawn must succeed");
}

fn start_bootstrap_chain(
    launcher: &dyn UserProcessLauncher,
    image: Option<UserlandImage>,
    user_va_end: usize,
) -> ! {
    let Some(image) = image else {
        warn!("userland blob missing; bootstrap process not started");
        power::system_off(1)
    };

    let launch = match spawn_process(launcher, image.bytes, image.phys_base, user_va_end) {
        Ok(launch) => launch,
        Err(e) => {
            warn!("bootstrap process spawn failed: {:?}", e);
            power::system_off(1)
        }
    };

    info!("bootstrap process spawned:");

    let port = launch.port;
    let irq_control = launch.irq_control;
    let image_region = launch.image_region;
    if let Err(e) = syscall_bridge::scheduler().spawn(
        SpawnConfig::new("bootstrap-log").priority(Priority::normal()),
        move || run_bootstrap(&port, irq_control, image_region),
    ) {
        warn!("bootstrap-log task spawn failed: {:?}", e);
        power::system_off(1)
    }

    let process_object = launch.info.process_object;
    let process_handle = match install_handle(Capability::new(
        CapabilityTarget::Process(process_object.clone()),
        Rights::READ,
    )) {
        Ok(id) => id,
        Err(e) => {
            warn!("init: install_handle failed: {:?}", e);
            power::system_off(1)
        }
    };

    if let Err(e) = signal_wait_one(process_handle, SIGNALED, None) {
        warn!("init: wait for bootstrap process exit failed: {:?}", e);
        power::system_off(1)
    }

    let exit_code = process_object.exit_code();
    info!("bootstrap process exited with code {}", exit_code);
    power::system_off(exit_code)
}
