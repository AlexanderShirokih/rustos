//! Программа на стандартной библиотеке устанавливает часы реального времени процесса по
//! сессии `Rtc` и выводит время суток `SystemTime`.

#![feature(molecular_ext)]

use core::error::Error;
use std::{
    os::molecular,
    time::{SystemTime, UNIX_EPOCH},
};

use components::{component_manifest, into_std_handle};
use rtc_abi::rtc;

component_manifest! {
    struct ClockStartup {
        requirements: { rtc: session(rtc::Protocol) },
    }
}

fn main() -> Result<(), Box<dyn Error>> {
    let rtc = components::requirements::<ClockStartup>()?.rtc.open()?;
    molecular::install_realtime_clock(into_std_handle(rtc.into_handle()))?;

    let seconds = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
    println!("system time {seconds}");
    Ok(())
}
