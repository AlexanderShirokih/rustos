#![allow(unsafe_code)]

extern crate alloc;

use alloc::{sync::Arc, vec::Vec};
use core::fmt::Display;

use drivers_common::scanner::EmbeddedDriversScanner;
use io::buffered_writer::BufferedWriter;
use klog::{fatal, info};
use scheduler::{ArchContext, ArchCpu, Bootstrapped, Scheduler, SchedulerConfig};

use crate::{
    driver_init::{InitSchedulerError, PendingDriver, run_retry_passes},
    irq_bridge,
    irq_control_adapter::InterruptsControlAdapter,
    kernel_context::KernelContext,
    power,
    scheduler_bootstrap::{BootstrapSchedulerError, KernelTimerSource, bootstrap_scheduler},
};

const BOOT_FAILURE_EXIT_CODE: i32 = 1;

/// Причина, по которой ядро не смогло дойти до передачи управления планировщику.
/// Все они невосстановимы и завершают загрузку в [`boot_failure`].
#[derive(Debug)]
pub enum BootError {
    /// Драйвер отказал фатально.
    DriverFatal {
        driver: &'static str,
        reason: alloc::string::String,
    },
    /// Остались драйверы с неразрешёнными зависимостями.
    UnresolvedDrivers,
    /// Не удалось создать сам драйвер из фабрики.
    DriverConstruction {
        driver: &'static str,
        reason: alloc::string::String,
    },
    /// Контроллер прерываний недоступен после инициализации драйверов.
    InterruptsUnavailable(drivers_common::BootServicesError),
    /// Не удалось поднять планировщик.
    Scheduler(BootstrapSchedulerError),
    /// Не удалось создать init-поток.
    InitProcess(scheduler::SpawnError),
    /// Планировщику нечего запускать: первый поток не выбрался.
    NoRunnableThread,
}

impl Display for BootError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            BootError::DriverFatal { driver, reason } => {
                write!(f, "driver '{driver}' initialization failed: {reason}")
            }
            BootError::UnresolvedDrivers => f.write_str("unresolved driver dependencies"),
            BootError::DriverConstruction { driver, reason } => {
                write!(f, "driver '{driver}' could not be constructed: {reason}")
            }
            BootError::InterruptsUnavailable(e) => {
                write!(
                    f,
                    "InterruptsService is unavailable after driver init: {e:?}"
                )
            }
            BootError::Scheduler(e) => write!(f, "scheduler bootstrap failed: {e}"),
            BootError::InitProcess(e) => write!(f, "init process spawn failed: {e:?}"),
            BootError::NoRunnableThread => f.write_str("no runnable thread to start"),
        }
    }
}

fn boot_failure(error: &BootError) -> ! {
    fatal!("Kernel boot failed: {error}");
    power::system_off(BOOT_FAILURE_EXIT_CODE)
}

/// Платформо-независимая точка входа ядра. После полного bootstrap вызывает `init_task` под
/// замаскированными IRQ - задача обязана зарегистрировать первый спавн-thread
/// (например, через [`Scheduler::spawn`]) и вернуть управление;
/// дальше kmain отдаёт CPU scheduler-у через [`Scheduler::start`].
pub fn kmain<A, F>(
    driver_scanner: EmbeddedDriversScanner,
    context: &mut KernelContext,
    kout: &BufferedWriter,
    scheduler_config: SchedulerConfig,
    init_task: F,
) -> !
where
    A: ArchContext,
    F: FnOnce(
        &Scheduler<A, KernelTimerSource, Bootstrapped>,
        &mut KernelContext,
    ) -> Result<(), scheduler::SpawnError>,
{
    match boot::<A, F>(driver_scanner, context, kout, scheduler_config, init_task) {
        // Infallible не имеет значений: ветка нужна только для исчерпывающего match.
        Ok(never) => match never {},
        Err(error) => {
            // Отказ мог случиться до штатного подключения консоли.
            bind_console(context, kout);
            boot_failure(&error)
        }
    }
}

fn boot<A, F>(
    driver_scanner: EmbeddedDriversScanner,
    context: &mut KernelContext,
    kout: &BufferedWriter,
    scheduler_config: SchedulerConfig,
    init_task: F,
) -> Result<core::convert::Infallible, BootError>
where
    A: ArchContext,
    F: FnOnce(
        &Scheduler<A, KernelTimerSource, Bootstrapped>,
        &mut KernelContext,
    ) -> Result<(), scheduler::SpawnError>,
{
    info!("Starting kmain");

    let pending = collect_into_pending_drivers(driver_scanner)?;

    run_all_drivers(context, pending)?;

    bind_console(context, kout);

    install_interrupts_hook(context)?;

    info!("Kernel drivers initialization completed");

    // Scheduler bootstrapится при замаскированных IRQ.
    <A::Cpu as ArchCpu>::disable_preemption();

    let scheduler =
        bootstrap_scheduler::<A>(context, scheduler_config).map_err(BootError::Scheduler)?;

    init_task(&scheduler, context).map_err(BootError::InitProcess)?;

    // Подъём идёт последним: стартующий CPU настраивает свой интерфейс
    // контроллера прерываний и сразу входит в планировщик, поэтому к этому
    // моменту готовы должны быть оба.
    <A::Cpu as ArchCpu>::start_secondaries();

    scheduler.start().map_err(|_| BootError::NoRunnableThread)
}

fn collect_into_pending_drivers(
    driver_scanner: EmbeddedDriversScanner,
) -> Result<Vec<PendingDriver>, BootError> {
    driver_scanner
        .into_iter()
        .map(|driver_handle| {
            let driver =
                driver_handle
                    .factory
                    .create()
                    .map_err(|reason| BootError::DriverConstruction {
                        driver: driver_handle.name,
                        reason,
                    })?;

            Ok(PendingDriver {
                name: driver_handle.name,
                driver,
            })
        })
        .collect()
}

fn run_all_drivers(
    kernel: &mut KernelContext,
    pending: Vec<PendingDriver>,
) -> Result<(), BootError> {
    match kernel.with_runtime_state(|caps, registry| run_retry_passes(pending, caps, registry)) {
        Ok(_) => Ok(()),

        Err(InitSchedulerError::Fatal { driver, reason }) => {
            Err(BootError::DriverFatal { driver, reason })
        }

        Err(InitSchedulerError::Unresolved { entries }) => {
            fatal!("Unresolved driver dependencies: {entries:?}");
            Err(BootError::UnresolvedDrivers)
        }
    }
}

fn bind_console(kernel: &mut KernelContext, buffered: &BufferedWriter) {
    kernel.with_runtime_state(|services, _| {
        if let Some(console) = services.console() {
            let console = console as Arc<dyn io::writer::Writer + Send + Sync>;
            // Сток паники указывает на драйвер консоли напрямую: путь паники
            // не проходит через локи `STDOUT` и `BufferedWriter`.
            klog::set_panic_stdout(console.clone());
            buffered.attach(&console);
        }
    });
}

fn install_interrupts_hook(kernel: &mut KernelContext) -> Result<(), BootError> {
    kernel.with_runtime_state(|services, _| {
        let interrupts = services
            .require_interrupts()
            .map_err(BootError::InterruptsUnavailable)?;

        interrupts.enable();
        irq_bridge::install_interrupts_service(interrupts.clone());
        // Архитектурно-независимый мост к контроллеру для IRQ-capability: тот же сервис,
        // обёрнутый адаптером. Это лишь `Once`-store, рантайм capability не нужен.
        capability::install_interrupts_control(Arc::new(InterruptsControlAdapter::new(
            interrupts.clone(),
        )));
        Ok(())
    })
}
