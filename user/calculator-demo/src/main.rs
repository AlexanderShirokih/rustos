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
use syscall::{Handle, Timeout};
use userland_image::decode;
use userland_loader::load_entry;

/// Значок сессии демо у калькулятора.
const CALCULATOR_SESSION_BADGE: u64 = 1;

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
    // SAFETY: регион образа ядро выдаёт только на чтение: отображений на запись
    // у него нет ни в одном адресном пространстве.
    let image = decode(unsafe { image_mapping.as_bytes() })?;

    // Порт для связи с сервисом калькулятора: корневая капа уезжает
    // калькулятору, у демо остаётся клиентский минт со значком сессии.
    let calculator_port = Port::create_service(CALCULATOR_SESSION_BADGE)?;
    let resource = Resource::self_resource()?;

    // Парсим контейнер initrd образа и получает из него программу
    let calculator_program = image
        .entry("/boot/calculator")
        .ok_or(Error::MissingEntry)?
        .program()?;

    // Спавним процесс с нашей программой. Связвываем его с нашим metering-resource и capability.
    let calculator_process = load_entry(&calculator_program, &resource, calculator_port.server)?;

    let calculator_client = calculator_port.client;

    // Транспорт держит сырой хэндл, поэтому не переживает клиентскую капу.
    {
        let calculator_api =
            CalculatorApiClient::new(PortTransport::client(calculator_client.handle()));
        for n in 1..=6 {
            let result = factorial(&calculator_api, n);
            log(&bootstrap_client, &format!("Factorial of {n} is {result}"));
        }
    }

    // Уход значка сессии завершает калькулятор, поэтому ждать его можно бессрочно.
    calculator_client.into_handle().close()?;
    calculator_process.join(Timeout::INFINITE)?;

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
