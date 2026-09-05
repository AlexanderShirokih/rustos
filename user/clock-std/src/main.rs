//! Компонент композиции на стандартной библиотеке: спрашивает время у драйвера RTC.
//!
//! Программа сама принимает стартовую транзакцию, тем же объявлением, что и программа без
//! стандартной библиотеки. Библиотека отдаёт ей одноразовый стартовый handle и принимает
//! лог-сессию набора под `stdout` и `stderr`.

use std::process;

use component_client::component_manifest;
use kernel_objects::PortTransport;
use rtc_abi::rtc;

const NO_STARTUP_EXIT_CODE: i32 = 1;

component_manifest! {
    struct ClockStartup {
        slots: { rtc: service(protocol = "Rtc") },
    }
}

fn main() {
    // До установки лог-сессии вывод не доходит до журнала, поэтому процесс называет отказ
    // приёма кодом выхода.
    let Ok(startup) = component_client_std::receive(ClockStartup::receive) else {
        process::exit(NO_STARTUP_EXIT_CODE);
    };

    let clock = rtc::Client::new(PortTransport::client(&startup.rtc));
    match clock.now() {
        Ok(response) => println!("rtc reads {}", response.value),
        Err(error) => println!("rtc call failed: {error:?}"),
    }
}
