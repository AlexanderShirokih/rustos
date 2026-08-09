//! Компонент композиции на стандартной библиотеке: спрашивает время у драйвера RTC.
//!
//! Свой манифест программа объявляет сама, и он перекрывает пустую запись от `std`. Стартовый
//! набор capability выдаёт `std::os::molecular` сырыми идентификаторами.

#![feature(molecular_ext)]

use std::os::molecular;

use molecular_rt::PortTransport;
use rtc_abi::rtc;
use syscall::Handle;

component_abi::component_manifest! {
    slots: { rtc: service(protocol = "Rtc") },
}

fn main() {
    let Some(startup) = molecular::startup() else {
        println!("no startup set: the process runs outside the composition");
        return;
    };
    let Some(session) = startup.slots().next() else {
        println!("no Rtc session in the startup set");
        return;
    };
    let clock = rtc::Client::new(PortTransport::client(Handle::from_nonzero(session)));
    match clock.now() {
        Ok(seconds) => println!("rtc reads {seconds}"),
        Err(error) => println!("rtc call failed: {error:?}"),
    }
}
