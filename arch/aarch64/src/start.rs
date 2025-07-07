#![cfg(target_arch = "aarch64")]
#![no_std]
#![no_main]

pub mod memory;
pub mod uart;

use core::arch::asm;

use crate::memory::setup::{MemorySetupError, setup_memory};
use crate::uart::Uart;
use arch_common::kernel::{BootInfo, Kernel};
use kernel_core::device::registry::DeviceRegistry;
use kernel_core::log;
use kernel_core::log::{Logger, OutputStreamLogger, set_logger};
use kernel_core::streams::OutputStreamExt;
use spin::Once;

#[unsafe(no_mangle)]
pub extern "C" fn _start() -> () {
    unsafe {
        asm!(
            // Init SP
            "ldr    x0, =_stack_top",
            "mov    sp, x0",
            // Enable FP/SIMD
            "mrs    x0, cpacr_el1",
            "orr    x0, x0, #(0x3 << 20)",
            "msr    cpacr_el1, x0",
            "isb",
            // Make a call here to keep the function clean until SP are set
            "b      early_main",
            options(noreturn)
        )
    }
}

static UART0: Once<Uart> = Once::new();
static DEV_REG: Once<DeviceRegistry> = Once::new();
static KERNEL: Once<Kernel> = Once::new();

#[unsafe(no_mangle)]
extern "C" fn early_main() {
    let uart0: &'static Uart = UART0.call_once(|| Uart::new(0x0900_0000));
    let registry: &'static DeviceRegistry = DEV_REG.call_once(|| DeviceRegistry::new(uart0));
    let static_kernel: &'static Kernel =
        KERNEL.call_once(|| Kernel::new(registry, OutputStreamLogger::new(uart0), BootInfo::new()));

    set_logger(static_kernel.logger());
    log::info("Starting kernel");

    if let Err(e) = setup_memory() {
        match e {
            MemorySetupError::AllocatorInitializationError => {
                uart0.write_str("Memory allocation fatal error").unwrap()
            }
            MemorySetupError::VirtualManagerSetupError => uart0
                .write_str("Virtual manager setup fatal error")
                .unwrap(),
        }
    } else {
        arch_common::start::main(static_kernel);
    }
}
