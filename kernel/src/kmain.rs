extern crate alloc;

use alloc::{sync::Arc, vec::Vec};

use drivers_common::{
    CapabilityStoreExt,
    scanner::EmbeddedDriversScanner,
    services::{
        console::ConsoleService,
        interrupts::InterruptsService,
        scheduler::{Priority, SchedulerService, SchedulerServiceExt, SpawnConfig},
    },
};
use io::buffered_writer::BufferedWriter;
use klog::info;

use crate::{
    driver_init::{InitSchedulerError, PendingDriver, run_retry_passes},
    irq_bridge,
    kernel_context::KernelContext,
    sched::{self, ArchContext, ArchCpu, Bootstrapped, KernelTimerSource, Scheduler, SchedulerConfig},
};

/// Платформо-независимая точка входа ядра
pub fn kmain<A>(
    driver_scanner: EmbeddedDriversScanner,
    context: &mut KernelContext,
    kout: &BufferedWriter,
    scheduler_config: SchedulerConfig,
) -> !
where
    A: ArchContext,
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

    let scheduler = sched::bootstrap_scheduler::<A>(context, scheduler_config);
    spawn_init_process(&scheduler, context);
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
            panic!("Unresolved driver dependencies: {:?}", entries);
        }
    }
}

fn bind_console(kernel: &mut KernelContext, buffered: &BufferedWriter) {
    kernel.with_runtime_state(|caps, _| {
        if let Ok(console) = caps.require_service::<dyn ConsoleService>() {
            buffered.attach(console as Arc<dyn io::writer::Writer + Send + Sync>);
        }
    });
}

fn install_interrupts_hook(kernel: &mut KernelContext) {
    kernel.with_runtime_state(|caps, _| {
        let interrupts = caps
            .require_service::<dyn InterruptsService>()
            .expect("InterruptsService must be available after driver initialization");

        interrupts.enable();
        irq_bridge::install_interrupts_service(interrupts.clone());
    });
}

fn spawn_init_process<A>(
    scheduler: &Scheduler<A, KernelTimerSource, Bootstrapped>,
    kernel: &mut KernelContext,
)
where
    A: ArchContext,
{
    let scheduler_service = kernel.with_runtime_state(|caps, _| {
        caps.require_service::<dyn SchedulerService>()
            .expect("SchedulerService must be registered before init task spawn")
    });

    scheduler
        .spawn(
            SpawnConfig::new("init").priority(Priority::highest()),
            move || spawn_demo_processes(scheduler_service),
        )
        .expect("init process spawn must succeed");
}

fn spawn_demo_processes(scheduler_service: Arc<dyn SchedulerService>) {
    for (idx, period_ms) in [(1_u32, 100_u64), (2, 300), (3, 700)] {
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
