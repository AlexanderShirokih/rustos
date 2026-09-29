//! Компонент композиции на стандартной библиотеке спрашивает время у драйвера RTC.
//!
//! Программа запрашивает сессию `rtc` у родителя тем же объявлением, что и программа без
//! стандартной библиотеки.

use core::error::Error;

use components::component_manifest;
use rtc_abi::rtc;

component_manifest! {
    struct ClockStartup {
        requirements: { rtc: session(rtc::Protocol) },
    }
}

fn main() -> Result<(), Box<dyn Error>> {
    let requirements = components::requirements::<ClockStartup>()?;
    let rtc = requirements.rtc.ok_or("RTC provider is unavailable")?;
    let seconds = rtc.client().now()?;
    println!("rtc reads {seconds}");
    Ok(())
}
