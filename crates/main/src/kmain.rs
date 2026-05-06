#![allow(unsafe_code)]

extern crate alloc;

use alloc::{sync::Arc, vec::Vec};

#[cfg(not(feature = "qemu-tests"))]
use drivers_common::services::scheduler::{
    Priority, SchedulerService, SchedulerServiceExt, SpawnConfig,
};
use drivers_common::{
    CapabilityStoreExt,
    scanner::EmbeddedDriversScanner,
    services::{console::ConsoleService, interrupts::InterruptsService},
};
use io::buffered_writer::BufferedWriter;
use klog::info;

use crate::{
    driver_init::{InitSchedulerError, PendingDriver, run_retry_passes},
    irq_bridge,
    kernel_context::KernelContext,
    sched::{
        self, ArchContext, ArchCpu, Bootstrapped, KernelTimerSource, Scheduler, SchedulerConfig,
    },
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

    #[cfg(feature = "qemu-tests")]
    spawn_qemu_tests_process(&scheduler, context);
    #[cfg(not(feature = "qemu-tests"))]
    spawn_init_process(&scheduler, context);

    scheduler.start()
}

#[cfg(feature = "qemu-tests")]
fn spawn_qemu_tests_process<A>(
    scheduler: &Scheduler<A, KernelTimerSource, Bootstrapped>,
    kernel: &mut KernelContext,
) where
    A: ArchContext,
{
    /// `*mut KernelContext` не Send автоматически; обёртка делает его
    /// перемещаемым в spawn-closure. Безопасность гарантируется тем, что
    /// `kmain` после spawn-а уходит в `scheduler.start()` и не
    /// обращается к контексту, а тестовый таск завершает QEMU через
    /// semihosting и тоже не возвращается.
    struct KernelCtxPtr(*mut KernelContext);

    // SAFETY: см. комментарий выше - единственный читатель указателя.
    unsafe impl Send for KernelCtxPtr {}

    let kernel_ptr = KernelCtxPtr(core::ptr::from_mut(kernel));

    scheduler
        .spawn(
            drivers_common::services::scheduler::SpawnConfig::new("qemu-tests")
                .priority(drivers_common::services::scheduler::Priority::highest()),
            move || {
                let captured = kernel_ptr;
                // SAFETY: см. комментарий выше.
                let kernel = unsafe { &mut *captured.0 };
                crate::qemu_tests::run(kernel)
            },
        )
        .expect("qemu-tests process spawn must succeed");
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

#[cfg(not(feature = "qemu-tests"))]
fn spawn_init_process<A>(
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
            move || spawn_demo_processes(scheduler_service),
        )
        .expect("init process spawn must succeed");
}

#[cfg(not(feature = "qemu-tests"))]
fn spawn_demo_processes(scheduler_service: Arc<dyn SchedulerService>) {
    use crate::timer_server::{pilot_client_subscribe, pilot_tick, spawn_timer_server};

    // TODO(kobject-migration): после миграции остальных сервисов на Timer KO
    // удалить ветку `legacy` и `dyn TimerService`.
    let client_end =
        spawn_timer_server(scheduler_service.clone()).expect("timer-server spawn must succeed");

    scheduler_service
        .spawn(
            SpawnConfig::new("test-process").priority(Priority::normal()),
            move || {
                let handles = match pilot_client_subscribe(client_end) {
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
