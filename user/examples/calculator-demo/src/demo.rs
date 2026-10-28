use core::error::Error;

use calculator_demo::calculator;
use components::component_manifest;
use executor::{Executor, PortChannel};
use ipc::Timeout;
use rtc_abi::rtc;

component_manifest! {
    struct DemoStartup {
        requirements: {
            calculator: session(calculator::Protocol),
            clock: session(rtc::Protocol),
        },
    }
}

pub(crate) fn main() -> Result<(), Box<dyn Error>> {
    let requirements = components::requirements::<DemoStartup>()?;
    let executor = Executor::new()?;

    executor.block_on(async {
        let calculator = PortChannel::client(requirements.calculator.open_async().await?);
        let clock = PortChannel::client(requirements.clock.open_async().await?);

        println!("started");
        let mut step: u64 = 1;
        let multiplier = 7;

        loop {
            let product = calculator.multiply(step, multiplier).await?;
            let now = clock.now().await?;
            println!("{step} * {multiplier} = {product}, rtc {now}s");

            executor::sleep(Timeout::from_secs(1)).await;
            step = step.wrapping_add(1);
        }
    })
}
