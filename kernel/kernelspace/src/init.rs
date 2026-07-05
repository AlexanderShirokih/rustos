//! Production-init: запускает bootstrap-цепочку userland.
//!
//! Передаётся в [`kmain`](crate::kmain::kmain) как init-таск; завершение
//! bootstrap-процесса выключает машину с его exit code.

extern crate alloc;

use alloc::{sync::Arc, vec::Vec};

use capability::{Capability, CapabilityTarget, Rights, SIGNALED, install_handle, signal_wait_one};
use klog::{info, warn};
use memory::MemoryRegion;
use scheduler::{ArchContext, Bootstrapped, Priority, Scheduler, SchedulerServiceExt, SpawnConfig};

use crate::{
    bootstrap::{close_bootstrap_port, run_bootstrap, spawn_process},
    kernel_context::{BootDtb, KernelContext, UserlandImage},
    power,
    scheduler_bootstrap::KernelTimerSource,
    syscall_bridge,
    user_process::{SchedulerUserProcessLauncher, UserProcessLauncher},
};

/// Бюджет закрытия лог-порта.
const KLOG_CLOSE_TIMEOUT_NS: u64 = 1_000_000_000;

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
    let dtb = kernel.dtb();
    let device_regions = kernel.device_regions();
    let user_va_end = A::USER_VA_END;

    scheduler
        .spawn(
            SpawnConfig::new("init").priority(Priority::highest()),
            move || {
                start_bootstrap_chain(launcher.as_ref(), image, dtb, device_regions, user_va_end)
            },
        )
        .expect("init process spawn must succeed");
}

fn start_bootstrap_chain(
    launcher: &dyn UserProcessLauncher,
    image: Option<UserlandImage>,
    dtb: BootDtb,
    device_regions: Vec<Arc<MemoryRegion>>,
    user_va_end: usize,
) -> ! {
    let exit_code = bootstrap_chain(launcher, image, dtb, device_regions, user_va_end).unwrap_or(1);
    power::system_off(exit_code)
}

fn bootstrap_chain(
    launcher: &dyn UserProcessLauncher,
    image: Option<UserlandImage>,
    dtb: BootDtb,
    device_regions: Vec<Arc<MemoryRegion>>,
    user_va_end: usize,
) -> Result<i32, ()> {
    let image =
        image.ok_or_else(|| warn!("userland blob missing; bootstrap process not started"))?;

    let launch = spawn_process(
        launcher,
        image.bytes,
        image.phys_base,
        dtb,
        device_regions,
        user_va_end,
    )
    .map_err(|e| warn!("bootstrap process spawn failed: {:?}", e))?;

    info!("bootstrap process spawned:");

    let server_port = launch.port.clone();
    let resources = launch.resources;
    syscall_bridge::scheduler()
        .spawn(
            SpawnConfig::new("bootstrap-log").priority(Priority::normal()),
            move || run_bootstrap(&server_port, resources),
        )
        .map_err(|e| warn!("bootstrap-log task spawn failed: {:?}", e))?;

    let process_object = launch.info.process_object;
    let process_handle = install_handle(Capability::new(
        CapabilityTarget::Process(process_object.clone()),
        Rights::READ,
    ))
    .map_err(|e| warn!("init: install_handle failed: {:?}", e))?;

    signal_wait_one(process_handle, SIGNALED, None)
        .map_err(|e| warn!("init: wait for bootstrap process exit failed: {:?}", e))?;

    if let Err(e) = close_bootstrap_port(&launch.port, KLOG_CLOSE_TIMEOUT_NS) {
        warn!("init: bootstrap port close failed: {:?}", e);
    }

    let exit_code = process_object.exit_code();
    info!("bootstrap process exited with code {}", exit_code);
    Ok(exit_code)
}
