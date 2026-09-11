//! Демонстрационный компонент композиции: клиент калькулятора, клиент часов реального
//! времени и читатель собственной файловой области.
//!
//! На старте программа печатает запись `/banner` своего каталога. Дальше раз в секунду она
//! спрашивает произведение у калькулятора и время у часов. Обе величины уходят строкой в
//! журнал.
//!
//! Отказ калькулятора и отказ часов программа называет разными кодами выхода.

use core::time::Duration;
use std::{process, thread};

use calculator_demo::calculator;
use component_startup::component_manifest;
use filesystem_abi::{AccessMode, OpenFlags, Path, file_system};
use filesystem_client::FileSystem;
use kernel_objects::{Session, SessionTransport};
use rtc_abi::rtc;
use syscall::Timeout;

const CALL_TIMEOUT: Timeout = Timeout::from_secs(2);

const TURN: Duration = Duration::from_secs(1);

const MULTIPLIER: u64 = 7;

const BANNER_RECORD: Path<'static> = match Path::new("/banner") {
    Ok(record) => record,
    Err(_) => panic!("the banner path keeps the form of the contract"),
};

const BANNER_MAX: usize = 64;

const NO_STARTUP_EXIT_CODE: i32 = 1;
const CALCULATOR_LOST_EXIT_CODE: i32 = 3;
const CLOCK_LOST_EXIT_CODE: i32 = 4;

component_manifest! {
    struct DemoStartup {
        requirements: {
            calculator: session(calculator::Protocol),
            clock: session(rtc::Protocol),
            area: session(file_system::Protocol),
        },
    }
}

fn main() {
    // Без журнала вывод `println!` теряется, и отказ старта передаётся кодом выхода.
    let Ok(requirements) = component_startup_std::start(DemoStartup::start) else {
        process::exit(NO_STARTUP_EXIT_CODE);
    };

    let calculator = requirements.calculator.client().with_wait(CALL_TIMEOUT);
    let clock = requirements.clock.client().with_wait(CALL_TIMEOUT);

    println!("started");
    announce(requirements.area);
    process::exit(run(&calculator, &clock))
}

fn announce(area: Session<file_system::Protocol>) {
    // Запись лежит в собственном каталоге компонента. Область windowed-сессии совпадает с
    // этим каталогом, и имя отсчитывается от его корня.
    let mut bytes = [0; BANNER_MAX];
    match banner(area, &mut bytes) {
        Some(text) => println!("banner: {text}"),
        None => println!("banner unread"),
    }
}

fn banner(area: Session<file_system::Protocol>, bytes: &mut [u8; BANNER_MAX]) -> Option<&str> {
    let mut area = FileSystem::windowed(area).with_wait(CALL_TIMEOUT);
    let mut record = area
        .open(BANNER_RECORD, AccessMode::Read, OpenFlags::NONE)
        .ok()?;
    let count = record.read(bytes).ok()?;

    Some(str::from_utf8(bytes.get(..count)?).ok()?.trim_end())
}

fn run(
    calculator: &calculator::Client<SessionTransport>,
    clock: &rtc::Client<SessionTransport>,
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
        println!("{step} * {MULTIPLIER} = {product}, rtc {now}s");
        thread::sleep(TURN);
        step = step.wrapping_add(1);
    }
}
