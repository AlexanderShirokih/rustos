#![no_std]
#![no_main]
#![allow(unsafe_code)]

extern crate alloc;

use alloc::format;
use core::panic::PanicInfo;

use log_abi::LogClient;
use rtc_demo::{ClientState, ClockClient, Error, Result, dispatch_client_args};
use runtime::{PortTransport, Signal, Timeout, receive_startup, thread_exit};
use syscall::{Handle, SIGNALED};

/// Пауза между опросами, чтобы в логе были разные секунды.
const POLL_PERIOD: Timeout = Timeout::from_millis(1_500);

/// Локальный тег строк клиента: доверенный источник ему добавляет relay.
const LOG_TAG: &str = "clock";

#[unsafe(no_mangle)]
pub extern "C" fn _start(args_server: Handle) -> ! {
    thread_exit(u64::from(run(args_server).is_err()))
}

fn run(args_server: Handle) -> Result<()> {
    let ClientState { clock, log } =
        receive_startup(args_server, dispatch_client_args)?.ok_or(Error::Vend)?;

    let clock = ClockClient::new(PortTransport::client(&clock));
    let log = LogClient::new(PortTransport::client(&log));

    // Используем пустой сигнал как таймер
    let timer = Signal::create()?;
    for _ in 0..3 {
        let seconds = clock.now()?;
        let _ = log.write_str(LOG_TAG, &format!("clock: now() = {seconds}"));
        let _ = timer.wait(SIGNALED, POLL_PERIOD);
    }

    Ok(())
}

#[panic_handler]
fn panic(_info: &PanicInfo<'_>) -> ! {
    loop {
        core::hint::spin_loop();
    }
}
