//! Программа-фикстура границы полномочий лог-капы: пробует по стартовой капе
//! операции bootstrap-контракта и завершается кодом первой прошедшей.

#![no_std]
#![no_main]
#![allow(unsafe_code)]

runtime::process_heap!();

use core::panic::PanicInfo;

use bootstrap_abi::bootstrap;
use kernel_objects::OwnedHandle;
use log_abi::log;
use runtime::{PortTransport, thread_exit};
use syscall::Handle;

mod log_fixture_protocol;

use log_fixture_protocol::{
    EXIT_BOOT_FDT, EXIT_DEVICE_MEMORY, EXIT_IRQ_CONTROL, EXIT_LOG_REFUSED, EXIT_USERLAND_IMAGE,
    FIXTURE_LINE,
};

/// `log` приходит в x0 как HandleId лог-сессии.
#[unsafe(no_mangle)]
pub extern "C" fn _start(log: Handle) -> ! {
    let log = OwnedHandle::from_handle(log);
    thread_exit(probe(&log))
}

fn probe(log: &OwnedHandle) -> u64 {
    let bootstrap = bootstrap::Client::new(PortTransport::client(log.borrow()));
    // Выданную capability закрывает ответ: пробе довольно самого факта выдачи.
    if let Ok(response) = bootstrap.acquire_irq_control()
        && response.value.is_ok()
    {
        return EXIT_IRQ_CONTROL;
    }
    if let Ok(response) = bootstrap.acquire_userland_image()
        && response.value.is_ok()
    {
        return EXIT_USERLAND_IMAGE;
    }
    if let Ok(response) = bootstrap.acquire_boot_fdt()
        && response.value.is_ok()
    {
        return EXIT_BOOT_FDT;
    }
    if let Ok(response) = bootstrap.acquire_device_memory(0)
        && response.value.is_ok()
    {
        return EXIT_DEVICE_MEMORY;
    }

    let client = log::Client::new(PortTransport::client(log.borrow()));
    if client.write_str("probe", FIXTURE_LINE).is_err() {
        return EXIT_LOG_REFUSED;
    }
    0
}

#[panic_handler]
fn panic(_info: &PanicInfo<'_>) -> ! {
    loop {
        core::hint::spin_loop();
    }
}
