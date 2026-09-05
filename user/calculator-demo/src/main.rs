//! Демо композиции: компонент-клиент двух сервисов и своей файловой области.
//! Читает запись собственного каталога сессией файловой системы, а дальше раз в
//! секунду спрашивает произведение у калькулятора, время у часов реального
//! времени и кладёт обе величины строкой в журнал.

use std::process;

use calculator_demo::calculator;
use component_abi::supervisor_gone;
use component_client::component_manifest;
use filesystem_abi::{AccessMode, OpenFlags, Path};
use filesystem_client::FileSystem;
use ipc::{Transport as _, wire::IpcError};
use kernel_objects::{OwnedHandle, PortTransport, ReceivedFrame};
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
const SUPERVISOR_LOST_EXIT_CODE: i32 = 7;

component_manifest! {
    struct DemoStartup {
        slots: {
            calculator: service(protocol = "Calculator"),
            clock: service(protocol = "Rtc"),
            area: directory(instance = "filesystem-imagefs"),
        },
    }
}

enum Supervisor {
    Alive,
    Gone,
}

fn main() {
    // До установки лог-сессии вывод не доходит до журнала, поэтому процесс называет отказ
    // приёма кодом выхода.
    let Ok(startup) = component_client_std::receive(DemoStartup::receive) else {
        process::exit(NO_STARTUP_EXIT_CODE);
    };

    let calculator =
        calculator::Client::new(PortTransport::client(&startup.calculator)).with_wait(CALL_TIMEOUT);
    let clock = rtc::Client::new(PortTransport::client(&startup.clock)).with_wait(CALL_TIMEOUT);

    println!("started");
    announce(startup.area);
    process::exit(run(
        &calculator,
        &clock,
        &PortTransport::server(&startup.control),
    ))
}

fn announce(area: OwnedHandle) {
    // Запись лежит в собственном каталоге компонента, и его область - корень
    // сессии: имя отсчитывается от неё.
    let mut bytes = [0; BANNER_MAX];
    match banner(area, &mut bytes) {
        Some(text) => println!("banner: {text}"),
        None => println!("banner unread"),
    }
}

fn banner(area: OwnedHandle, bytes: &mut [u8; BANNER_MAX]) -> Option<&str> {
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
    control: &PortTransport,
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
        if matches!(turn(control), Supervisor::Gone) {
            println!("supervisor gone");
            return SUPERVISOR_LOST_EXIT_CODE;
        }
        step = step.wrapping_add(1);
    }
}

fn turn(control: &PortTransport) -> Supervisor {
    // Срок приёма на control-порту задаёт такт цикла, а уход значка supervisor
    // виден отказом того же приёма.
    match control.wait_readable(TURN) {
        // Control-порт протокола не обслуживает, и пришедший кадр уходит без ответа.
        // Владеющий кадр закрывает его ресурсы и будит вызывателя.
        Ok(()) => {
            drop(ReceivedFrame::take(control));
            Supervisor::Alive
        }
        // Покадровое исчерпание такт не рвёт, как и уход чужого значка.
        Err(IpcError::Timeout | IpcError::ResourceExhausted) => Supervisor::Alive,
        Err(IpcError::SenderGone) if !supervisor_gone(control.peer_badge()) => Supervisor::Alive,
        Err(_) => Supervisor::Gone,
    }
}
