extern crate alloc;

use crate::driver_init::{run_retry_passes, InitSchedulerError, PendingDriver};
use crate::irq_bridge;
use crate::kernel_context::KernelContext;
use alloc::vec::Vec;
use drivers_common::services::console::ConsoleService;
use drivers_common::CapabilityStoreExt;
use drivers_common::scanner::DriverScanner;
use drivers_common::services::interrupts::InterruptsService;
use drivers_common::services::timer::TimerService;
use io::buffered_writer::BufferedWriter;
use klog::{debug, info};

/// Главная функция ядра
pub fn kmain(
    driver_scanner: DriverScanner,
    kernel: &mut KernelContext,
    kout: &BufferedWriter,
) {
    info!("Starting kmain");

    let pending = collect_pending_drivers(driver_scanner);

    run_all_drivers(kernel, pending);

    // Привязка консоли и flush буфера
    bind_console(kernel, kout);

    install_interrupts_hook(kernel);
    smoke_check_timer_ticks(kernel);

    info!("Kernel drivers initialization completed")
}

fn collect_pending_drivers(driver_scanner: DriverScanner) -> Vec<PendingDriver> {
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
            let writer = console.writer();
            // Консоль живёт в capabilities, writer — ссылка на неё. Для attach нужен &'static.
            // SAFETY: console — Arc в capabilities, не будет dropped. writer() возвращает &T где T: ConsoleService.
            let writer_static: &'static (dyn io::writer::Writer + Sync) =
                unsafe { core::mem::transmute(writer) };
            buffered.attach(writer_static);
        }
    });
}

fn install_interrupts_hook(kernel: &mut KernelContext) {
    // Мост устанавливается после инициализации runtime-драйверов.
    kernel.with_runtime_state(|caps, _| {
        let interrupts = caps
            .require_service::<dyn InterruptsService>()
            .expect("InterruptsService must be available after driver initialization");

        interrupts.enable();
        irq_bridge::install_interrupts_service(interrupts.clone());
    });
}

fn smoke_check_timer_ticks(kernel: &mut KernelContext) {
    kernel.with_runtime_state(|caps, _| {
        let timer = caps
            .require_service::<dyn TimerService>()
            .expect("TimerService must be available after driver initialization");

        let elapsed_time_ms = timer.time_monotonic_elapsed() / 1_000_000;

        timer.set_periodic(100u64);

        debug!("elapsed_time_ms: {elapsed_time_ms}");
    });
}
