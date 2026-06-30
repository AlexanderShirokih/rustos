#![no_std]
#![no_main]
#![allow(unsafe_code)]

extern crate alloc;

use alloc::format;
use core::panic::PanicInfo;

use bootstrap::BootstrapClient;
use rtc_demo::{ClientState, ClockClient, Error, Result, dispatch_client_args};
use runtime::{PortTransport, Signal, Timeout, thread_exit};
use syscall::{Handle, SIGNALED};

/// Пауза между опросами, чтобы в логе были разные секунды.
const POLL_PERIOD: Timeout = Timeout::from_ns(1_500_000_000);

const LOG_TAG: &str = "rtc-client";

#[unsafe(no_mangle)]
pub extern "C" fn _start(args_server: Handle) -> ! {
    thread_exit(u64::from(run(args_server).is_err()))
}

fn run(args_server: Handle) -> Result<()> {
    let args_transport = PortTransport::server(args_server);
    let mut args: Option<ClientState> = None;

    dispatch_client_args(&mut args, &args_transport)?;
    let ClientState { clock, klog } = args.ok_or(Error::Vend)?;

    let clock = ClockClient::new(PortTransport::client(&clock));
    let klog = BootstrapClient::new(PortTransport::client(&klog));

    // Используем пустой сигнал как таймер
    let timer = Signal::create()?;
    for _ in 0..3 {
        let seconds = clock.now()?;
        let _ = klog.log_str(LOG_TAG, &format!("clock: now() = {seconds}"));
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
