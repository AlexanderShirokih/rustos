//! Демонстрационный компонент композиции: клиент калькулятора и клиент часов реального
//! времени.
//!
//! Раз в секунду программа спрашивает произведение у калькулятора и время у часов. Обе
//! величины уходят строкой в журнал.

use core::{error::Error, time::Duration};
use std::thread;

use calculator_demo::calculator;
use components::component_manifest;
use ipc::wire::IpcError;
use kernel_objects::SessionTransport;
use rtc_abi::rtc;
use syscall::Timeout;

const CALL_TIMEOUT: Timeout = Timeout::from_secs(2);

component_manifest! {
    struct DemoStartup {
        requirements: {
            calculator: session(calculator::Protocol),
            clock: session(rtc::Protocol),
        },
    }
}

fn main() -> Result<(), Box<dyn Error>> {
    let requirements = components::requirements::<DemoStartup>()?;

    let calculator = requirements
        .calculator
        .ok_or("calculator provider is unavailable")?;
    let clock = requirements.clock.ok_or("RTC provider is unavailable")?;

    let calculator = calculator.client().with_wait(CALL_TIMEOUT);
    let clock = clock.client().with_wait(CALL_TIMEOUT);

    println!("started");
    run(&calculator, &clock)?;
    Ok(())
}

fn run(
    calculator: &calculator::Client<SessionTransport>,
    clock: &rtc::Client<SessionTransport>,
) -> Result<(), IpcError> {
    let mut step: u64 = 1;
    let multiplier = 7;

    loop {
        let product = calculator.multiply(step, multiplier)?;
        let now = clock.now()?;

        println!("{step} * {multiplier} = {product}, rtc {now}s");
        thread::sleep(Duration::from_secs(1));

        step = step.wrapping_add(1);
    }
}
