extern crate alloc;

use crate::driver_init::{InitSchedulerError, PendingDriver, run_retry_passes};
use crate::irq_bridge;
use crate::kernel_context::KernelContext;
use alloc::vec::Vec;
use drivers_common::CapabilityStoreExt;
use drivers_common::scanner::DriverScanner;
use drivers_common::services::interrupts::InterruptsService;
use klog::info;

/// Главная функция ядра
pub fn kmain(driver_scanner: DriverScanner, kernel: &mut KernelContext) {
    info!("Starting kmain");

    let pending = collect_pending_drivers(driver_scanner);

    run_all_drivers(kernel, pending);

    install_interrupts_hook(kernel);

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

fn install_interrupts_hook(kernel: &mut KernelContext) {
    // Мост устанавливается после инициализации runtime-драйверов.
    kernel.with_runtime_state(|caps, _| {
        let interrupts = caps
            .require_service::<dyn InterruptsService>()
            .expect("InterruptsService must be available after driver initialization");

        irq_bridge::install_interrupts_service(interrupts.clone());
        interrupts.enable();
    });
}
