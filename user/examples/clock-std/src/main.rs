//! Компонент композиции на стандартной библиотеке спрашивает время у драйвера RTC.
//!
//! Программа запрашивает сессию `rtc` у родителя тем же объявлением, что и программа без
//! стандартной библиотеки.

use std::process;

use component_startup::component_manifest;
use rtc_abi::rtc;

const NO_STARTUP_EXIT_CODE: i32 = 1;
const RTC_CALL_FAILED_EXIT_CODE: i32 = 2;

component_manifest! {
    struct ClockStartup {
        requirements: { rtc: session(rtc::Protocol) },
    }
}

fn main() {
    // Без журнала вывод `println!` теряется, и отказ старта передаётся кодом выхода.
    let Ok(requirements) = component_startup_std::start(ClockStartup::start) else {
        process::exit(NO_STARTUP_EXIT_CODE);
    };

    match requirements.rtc.client().now() {
        Ok(seconds) => println!("rtc reads {seconds}"),
        Err(error) => {
            println!("rtc call failed: {error:?}");
            process::exit(RTC_CALL_FAILED_EXIT_CODE);
        }
    }
}
