extern crate alloc;

use crate::kernel_context::KernelContext;
use drivers_common::scanner::DriverScanner;
use drivers_common::{DriverContext, RuntimeRequestApplier};
use klog::debug;

/// Главная функция ядра
pub fn kmain(driver_scanner: DriverScanner, kernel: &mut KernelContext) {
    debug!("Starting kmain");

    let ops = RuntimeRequestApplier {
        memory_mapper: kernel.memory_mapper(),
    };

    for driver_factory in driver_scanner.into_iter() {
        let mut context = DriverContext::new(&ops);

        driver_factory
            .create(&mut context)
            .expect("Failed to initialize kernel driver");
    }
}
