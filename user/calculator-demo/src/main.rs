//! Демонстрационный компонент композиции: клиент калькулятора, клиент часов реального
//! времени и читатель собственной файловой области.
//!
//! На старте программа печатает запись `/banner` своего каталога. Дальше раз в секунду она
//! спрашивает произведение у калькулятора и время у часов. Обе величины уходят строкой в
//! журнал.
//!
//! Отказ каждого из трёх контрагентов программа называет своим кодом выхода.

use std::process;

use calculator_demo::calculator;
use component_startup::component_manifest;
use filesystem_abi::{AccessMode, OpenFlags, Path};
use filesystem_client::FileSystem;
use ipc::{Transport as _, wire::IpcError};
use kernel_objects::{Port, PortTransport, ReceivedFrame};
use rtc_abi::rtc;
use syscall::Timeout;

const CALL_TIMEOUT: Timeout = Timeout::from_secs(2);

const TURN: Timeout = Timeout::from_secs(1);

const MULTIPLIER: u64 = 7;

const BANNER_RECORD: Path<'static> = match Path::new("/banner") {
    Ok(record) => record,
    Err(_) => panic!("the banner path keeps the form of the contract"),
};

const BANNER_MAX: usize = 64;

const NO_STARTUP_EXIT_CODE: i32 = 1;
const CALCULATOR_LOST_EXIT_CODE: i32 = 3;
const CLOCK_LOST_EXIT_CODE: i32 = 4;
const STARTUP_PORT_FAILED_EXIT_CODE: i32 = 7;

component_manifest! {
    struct DemoStartup {
        slots: {
            calculator: service(protocol = "Calculator"),
            clock: service(protocol = "Rtc"),
            area: directory(instance = "filesystem-imagefs"),
        },
    }
}

fn main() {
    // До установки лог-сессии вывод не доходит до журнала, поэтому процесс называет отказ
    // приёма кодом выхода.
    let Ok(resources) = component_startup_std::receive(DemoStartup::receive) else {
        process::exit(NO_STARTUP_EXIT_CODE);
    };

    let calculator = calculator::Client::new(PortTransport::client(&resources.calculator))
        .with_wait(CALL_TIMEOUT);
    let clock = rtc::Client::new(PortTransport::client(&resources.clock)).with_wait(CALL_TIMEOUT);

    println!("started");
    announce(resources.area);
    process::exit(run(
        &calculator,
        &clock,
        &PortTransport::server(&resources.startup),
    ))
}

fn announce(area: Port) {
    // Запись лежит в собственном каталоге компонента. Область windowed-сессии совпадает с
    // этим каталогом, и имя отсчитывается от его корня.
    let mut bytes = [0; BANNER_MAX];
    match banner(area, &mut bytes) {
        Some(text) => println!("banner: {text}"),
        None => println!("banner unread"),
    }
}

fn banner(area: Port, bytes: &mut [u8; BANNER_MAX]) -> Option<&str> {
    let mut area = FileSystem::windowed(area).with_wait(CALL_TIMEOUT);
    let mut record = area
        .open(BANNER_RECORD, AccessMode::Read, OpenFlags::NONE)
        .ok()?;
    let count = record.read(bytes).ok()?;

    Some(str::from_utf8(bytes.get(..count)?).ok()?.trim_end())
}

fn run(
    calculator: &calculator::Client<PortTransport>,
    clock: &rtc::Client<PortTransport>,
    startup: &PortTransport,
) -> i32 {
    let mut step: u64 = 1;
    loop {
        // Уход провайдера виден отказом транспорта на его сессии.
        let Ok(product) = calculator.multiply(step, MULTIPLIER) else {
            println!("calculator gone");
            return CALCULATOR_LOST_EXIT_CODE;
        };
        let Ok(now) = clock.now() else {
            println!("clock gone");
            return CLOCK_LOST_EXIT_CODE;
        };
        println!(
            "{step} * {MULTIPLIER} = {}, rtc {}s",
            product.value, now.value
        );
        if !turn(startup) {
            println!("startup port failed");
            return STARTUP_PORT_FAILED_EXIT_CODE;
        }
        step = step.wrapping_add(1);
    }
}

fn turn(startup: &PortTransport) -> bool {
    // Срок приёма на startup-порту задаёт такт цикла.
    match startup.wait_readable(TURN) {
        // Кадр стартовой транзакции уже принят, и пришедший на рабочей фазе уходит без
        // ответа. Владеющий кадр закрывает его ресурсы и будит вызывателя.
        Ok(()) => {
            drop(ReceivedFrame::take(startup));
            true
        }
        // Покадровое исчерпание такт не рвёт, как и уход значка потребителя.
        Err(IpcError::Timeout | IpcError::ResourceExhausted | IpcError::SenderGone) => true,
        Err(_) => false,
    }
}
