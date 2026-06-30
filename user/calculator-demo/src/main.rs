#![no_std]
#![no_main]
#![allow(unsafe_code)]

extern crate alloc;

use alloc::format;
use core::panic::PanicInfo;

use bootstrap::BootstrapClient;
use calculator_demo::{CalculatorApiClient, Error, Result};
use runtime::{
    MemoryRegion, OwnedHandle, Port, PortTransport, Resource, UserMemFlags, thread_exit,
};
use syscall::{Handle, Rights, Timeout};
use userland_image::decode;
use userland_loader::load_entry;

#[unsafe(no_mangle)]
pub extern "C" fn _start(root: Handle) -> ! {
    thread_exit(u64::from(run(root).is_err()))
}

fn run(root: Handle) -> Result<()> {
    let root_capability = OwnedHandle::from_handle(root);

    let borrowed_root = root_capability.borrow();
    let bootstrap_client = BootstrapClient::new(PortTransport::client(borrowed_root));

    // Получаем capability на доступ к userland-image
    let image_cap = bootstrap_client
        .acquire_userland_image()?
        .map_err(|_| Error::Vend)?;

    // Отображаем userland-image в своё адресное пространство
    let image_region = MemoryRegion::adopt(image_cap);
    let image_mapping = image_region.map_full(UserMemFlags::ReadOnly)?;

    // Парсим контейнер userland-image
    let image = decode(image_mapping.as_bytes())?;

    // Порт для связи с сервисом калькулятора
    let calculator_port = Port::create()?;
    let resource = Resource::self_resource();

    // Парсим контейнер initrd образа и получает из него программу
    let calculator_entry = image.entry("calculator").ok_or(Error::MissingEntry)?;

    // Калькулятору нужны READ (recv как сервер), WRITE (reply) и TRANSFER
    // (чтобы load_entry передал хэндл потомку).
    let calculator_capability = calculator_port.handle();
    let duplicated_capability =
        calculator_capability.duplicate(Rights::READ | Rights::WRITE | Rights::TRANSFER, 0)?;

    // Спавним процесс с нашей программой. Связвываем его с нашим metering-resource и capability.
    let calculator_process = load_entry(&calculator_entry, &resource, duplicated_capability)?;

    let calculator_api = CalculatorApiClient::new(PortTransport::client(calculator_capability));

    // Выполняем синхронный вызов через IPC канал
    for n in 1..=6 {
        let result = factorial(&calculator_api, n);
        log(&bootstrap_client, &format!("Factorial of {n} is {result}"));
    }

    let _ = calculator_process.join(Timeout::from_ns(500_000));

    Ok(())
}

fn factorial(calculator: &CalculatorApiClient<PortTransport>, n: u64) -> u64 {
    if n <= 1 {
        1
    } else {
        calculator
            .multiply(n, factorial(calculator, n - 1))
            .unwrap()
    }
}

fn log(bootstrap_client: &BootstrapClient<PortTransport>, msg: &str) {
    let _ = bootstrap_client.log_str("calculator", msg);
}

#[panic_handler]
fn panic(_info: &PanicInfo<'_>) -> ! {
    loop {
        core::hint::spin_loop();
    }
}
