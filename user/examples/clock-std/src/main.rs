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
    let rtc = components::requirements::<ClockStartup>()?.rtc.open()?;
    let seconds = rtc.client().now()?;
    println!("rtc reads {seconds}");
    Ok(())
}
