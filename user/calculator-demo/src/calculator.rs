#![no_std]
#![no_main]
#![allow(unsafe_code)]

extern crate alloc;

use core::panic::PanicInfo;

use calculator_demo::{CalculatorApiService, Result, dispatch_calculator_api};
use runtime::{PortTransport, thread_exit};
use syscall::Handle;

struct Calculator;

impl CalculatorApiService for Calculator {
    fn multiply(&mut self, a: u64, b: u64) -> u64 {
        a * b
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn _start(root: Handle) -> ! {
    thread_exit(u64::from(run(root).is_err()))
}

fn run(root: Handle) -> Result<()> {
    let calc_api_transport = PortTransport::server(root);

    let mut calculator = Calculator;

    loop {
        let _ = dispatch_calculator_api(&mut calculator, &calc_api_transport);
    }
}

#[panic_handler]
fn panic(_info: &PanicInfo<'_>) -> ! {
    loop {
        core::hint::spin_loop();
    }
}
