#![no_std]
#![no_main]
#![allow(unsafe_code)]

use core::panic::PanicInfo;

use io::mmio::{Mmio, Reg};
use rtc_demo::{ClockService, ClockState, Error, Result, dispatch_clock, dispatch_driver_args};
use runtime::{MemoryRegion, PortTransport, UserMemFlags, thread_exit};
use syscall::Handle;

/// Текущее время (секунды), RO.
const RTC_DR: Reg<u32> = Reg::new(0x00);

/// Сервер часов поверх отображённого окна PL031.
struct ClockDriver {
    device: Mmio,
}

impl ClockDriver {
    fn new(device: Mmio) -> Self {
        Self { device }
    }
}

impl ClockService for ClockDriver {
    fn now(&mut self) -> u64 {
        u64::from(self.device.read_reg(RTC_DR))
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn _start(server_args: Handle) -> ! {
    thread_exit(u64::from(run(server_args).is_err()))
}

fn run(server_args: Handle) -> Result<()> {
    let args_transport = PortTransport::server(server_args);
    let mut args: Option<ClockState> = None;

    dispatch_driver_args(&mut args, &args_transport)?;
    let ClockState { device, clock_port } = args.ok_or(Error::Vend)?;

    // Отображаем устройство в своё адресное пространство.
    let device = MemoryRegion::from_handle(device);
    let mapping = device.map_full(UserMemFlags::ReadWrite)?;
    let mut driver = ClockDriver::new(mapping.mmio());

    let clock_transport = PortTransport::server(&clock_port);
    while dispatch_clock(&mut driver, &clock_transport).is_ok() {}
    Ok(())
}

#[panic_handler]
fn panic(_info: &PanicInfo<'_>) -> ! {
    loop {
        core::hint::spin_loop();
    }
}
