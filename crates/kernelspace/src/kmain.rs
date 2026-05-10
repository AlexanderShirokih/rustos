#![allow(unsafe_code)]

extern crate alloc;

use alloc::{sync::Arc, vec::Vec};

use drivers_common::scanner::EmbeddedDriversScanner;
use io::buffered_writer::BufferedWriter;
use klog::info;
use scheduler::{ArchContext, ArchCpu, Bootstrapped, Scheduler, SchedulerConfig};

use crate::{
    driver_init::{InitSchedulerError, PendingDriver, run_retry_passes},
    irq_bridge,
    kernel_context::KernelContext,
    scheduler_bootstrap::{KernelTimerSource, bootstrap_scheduler},
};

/// Платформо-независимая точка входа ядра. После полного bootstrap-а
/// (драйверы, console, interrupts, scheduler) вызывает `init_task` под
/// замаскированными IRQ - задача обязана зарегистрировать первый
/// спавн-thread (например, через [`Scheduler::spawn`]) и вернуть
/// управление; дальше kmain отдаёт CPU scheduler-у через
/// [`Scheduler::start`].
pub fn kmain<A, F>(
    driver_scanner: EmbeddedDriversScanner,
    context: &mut KernelContext,
    kout: &BufferedWriter,
    scheduler_config: SchedulerConfig,
    init_task: F,
) -> !
where
    A: ArchContext,
    F: FnOnce(&Scheduler<A, KernelTimerSource, Bootstrapped>, &mut KernelContext),
{
    info!("Starting kmain");

    let pending = collect_into_pending_drivers(driver_scanner);

    run_all_drivers(context, pending);

    bind_console(context, kout);

    install_interrupts_hook(context);

    info!("Kernel drivers initialization completed");

    // Гарантия: scheduler bootstrap'ится при замаскированных IRQ. Тики
    // обработчика прилетят только после первого `enable_preemption()` внутри
    // trampoline уже выбранного потока.
    <A::Cpu as ArchCpu>::disable_preemption();

    let scheduler = bootstrap_scheduler::<A>(context, scheduler_config);

    init_task(&scheduler, context);

    scheduler.start()
}

fn collect_into_pending_drivers(driver_scanner: EmbeddedDriversScanner) -> Vec<PendingDriver> {
    driver_scanner
        .into_iter()
        .map(|driver_handle| {
            let driver = driver_handle
                .factory
                .create()
                .expect("Failed to initialize kernel driver");

            PendingDriver {
                name: driver_handle.name,
                driver,
            }
        })
        .collect()
}

fn run_all_drivers(kernel: &mut KernelContext, pending: Vec<PendingDriver>) {
    match kernel.with_runtime_state(|caps, registry| run_retry_passes(pending, caps, registry)) {
        Ok(_) => {}

        Err(InitSchedulerError::Fatal { driver, reason }) => {
            panic!("Driver initialization failed for {driver}: {reason}");
        }

        Err(InitSchedulerError::Unresolved { entries }) => {
            panic!("Unresolved driver dependencies: {entries:?}");
        }
    }
}

fn bind_console(kernel: &mut KernelContext, buffered: &BufferedWriter) {
    kernel.with_runtime_state(|services, _| {
        if let Some(console) = services.console() {
            buffered.attach(&(console as Arc<dyn io::writer::Writer + Send + Sync>));
        }
    });
}

fn install_interrupts_hook(kernel: &mut KernelContext) {
    kernel.with_runtime_state(|services, _| {
        let interrupts = services
            .require_interrupts()
            .expect("InterruptsService must be available after driver initialization");

        interrupts.enable();
        irq_bridge::install_interrupts_service(interrupts.clone());
    });
}
