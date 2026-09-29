//! Начальный процесс, который ведёт bootstrap-цепочку userland.
//!
//! [`spawn_initial_process`] передаётся в [`kmain`](crate::kmain::kmain). Завершение
//! bootstrap-процесса выключает машину с его exit code.

extern crate alloc;

use alloc::{sync::Arc, vec::Vec};

use capability::{Capability, CapabilityTarget, Rights, SIGNALED, install_handle, signal_wait_one};
use drivers_common::services::interrupts::IrqLineSpan;
use klog::{info, warn};
use memory::MemoryRegion;
use scheduler::{ArchContext, Bootstrapped, Priority, Scheduler, SchedulerServiceExt, SpawnConfig};

use crate::{
    bootstrap::{SERVER_STACK_PAGES, drain_bootstrap_port, run_bootstrap, spawn_process},
    kernel_context::{BootDtb, KernelContext, UserlandImage},
    power,
    scheduler_bootstrap::KernelTimerSource,
    syscall_bridge,
    user_process::{SchedulerUserProcessLauncher, UserProcessLauncher},
};

const KLOG_DRAIN_TIMEOUT_NS: u64 = 1_000_000_000;

/// Спавнит начальный процесс с приоритетом `highest`. Он ведёт
/// bootstrap-цепочку userland и выключает машину по её завершении.
pub fn spawn_initial_process<A>(
    scheduler: &Scheduler<A, KernelTimerSource, Bootstrapped>,
    kernel: &mut KernelContext,
) -> Result<(), scheduler::SpawnError>
where
    A: ArchContext,
{
    let launcher: Arc<dyn UserProcessLauncher> = Arc::new(SchedulerUserProcessLauncher::new(
        scheduler.handle(),
        kernel.address_space_factory(),
    ));

    let image = kernel.userland_image();
    let dtb = kernel.dtb();
    let device_regions = kernel.device_regions();
    let irq_lines = kernel.assignable_irq_lines();
    let user_va_end = A::USER_VA_END;

    scheduler
        .spawn(
            SpawnConfig::new("initial-process").priority(Priority::highest()),
            move || {
                start_bootstrap_chain(
                    launcher.as_ref(),
                    image,
                    dtb,
                    device_regions,
                    irq_lines,
                    user_va_end,
                )
            },
        )
        .map(|_| ())
}

fn start_bootstrap_chain(
    launcher: &dyn UserProcessLauncher,
    image: Option<UserlandImage>,
    dtb: BootDtb,
    device_regions: Vec<Arc<MemoryRegion>>,
    irq_lines: Option<IrqLineSpan>,
    user_va_end: usize,
) -> ! {
    let exit_code =
        bootstrap_chain(launcher, image, dtb, device_regions, irq_lines, user_va_end).unwrap_or(1);
    power::system_off(exit_code)
}

fn bootstrap_chain(
    launcher: &dyn UserProcessLauncher,
    image: Option<UserlandImage>,
    dtb: BootDtb,
    device_regions: Vec<Arc<MemoryRegion>>,
    irq_lines: Option<IrqLineSpan>,
    user_va_end: usize,
) -> Result<i32, ()> {
    let image =
        image.ok_or_else(|| warn!("userland blob missing; bootstrap process not started"))?;
    let irq_lines = irq_lines
        .ok_or_else(|| warn!("interrupts service missing; bootstrap process not started"))?;

    let launch = spawn_process(
        launcher,
        image.bytes,
        image.phys_base,
        dtb,
        device_regions,
        irq_lines,
        user_va_end,
    )
    .map_err(|e| warn!("bootstrap process spawn failed: {:?}", e))?;

    info!("bootstrap process spawned:");

    // Capability получателя действительна, пока работает сервер. Её уход переводит порт
    // в состояние без получателей.
    let receiver = launch.receiver;
    let port = launch.port.clone();
    let resources = launch.resources;
    syscall_bridge::scheduler()
        .ok_or_else(|| warn!("scheduler missing; bootstrap-log task not started"))?
        .spawn(
            SpawnConfig::new("bootstrap-log")
                .priority(Priority::normal())
                .stack_pages(SERVER_STACK_PAGES),
            move || run_bootstrap(&port, receiver, resources),
        )
        .map_err(|e| warn!("bootstrap-log task spawn failed: {:?}", e))?;

    let process_object = launch.info.process_object;
    let process_handle = install_handle(Capability::new(
        CapabilityTarget::Process(process_object.clone()),
        Rights::READ,
    ))
    .map_err(|e| warn!("initial-process: install_handle failed: {:?}", e))?;

    signal_wait_one(process_handle, SIGNALED, None)
        .map_err(|e| warn!("initial-process: wait for bootstrap process exit failed: {e:?}"))?;

    if let Err(e) = drain_bootstrap_port(&launch.port, KLOG_DRAIN_TIMEOUT_NS) {
        warn!("initial-process: bootstrap port drain failed: {:?}", e);
    }

    let exit_code = process_object.exit_code();
    info!("bootstrap process exited with code {}", exit_code);
    Ok(exit_code)
}
