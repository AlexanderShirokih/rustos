extern crate alloc;

use crate::kernel_context::KernelContext;
use drivers_common::scanner::DriverScanner;
use drivers_common::{DriverInitContext, RuntimeRequestApplier};
use klog::info;

/// Главная функция ядра
pub fn kmain(driver_scanner: DriverScanner, kernel: &mut KernelContext) {
    info!("Starting kmain");

    let ops = RuntimeRequestApplier {
        memory_mapper: kernel.memory_mapper(),
        linear_offset: kernel.base_offset(),
    };

    for driver_factory in driver_scanner.into_iter() {
        let mut context = DriverInitContext::new(&ops);

        let mut driver = driver_factory
            .create(&mut context)
            .expect("Failed to initialize kernel driver");

        driver.run().expect("Failed to run kernel driver");
    }

    info!("Kernel drivers initialization completed")
}
