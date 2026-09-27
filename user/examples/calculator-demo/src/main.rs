//! Демонстрационный компонент композиции: клиент калькулятора и клиент часов реального
//! времени.
//!
//! Раз в секунду программа спрашивает произведение у калькулятора и время у часов. Обе
//! величины уходят строкой в журнал.
//!
//! Отказ калькулятора и отказ часов программа называет разными кодами выхода.

use core::time::Duration;
use std::{process, thread};

use calculator_demo::calculator;
use component_startup::component_manifest;
use kernel_objects::SessionTransport;
use rtc_abi::rtc;
use syscall::Timeout;

const CALL_TIMEOUT: Timeout = Timeout::from_secs(2);

const TURN: Duration = Duration::from_secs(1);

const MULTIPLIER: u64 = 7;

const NO_STARTUP_EXIT_CODE: i32 = 1;
const CALCULATOR_LOST_EXIT_CODE: i32 = 3;
const CLOCK_LOST_EXIT_CODE: i32 = 4;

component_manifest! {
    struct DemoStartup {
        requirements: {
            calculator: session(calculator::Protocol),
            clock: session(rtc::Protocol),
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
    process::exit(run(&calculator, &clock))
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
