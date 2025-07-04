use crate::kernel::Kernel;
use crate::kernel::arch::aarch64::memory::memory::MemoryLayout;
use crate::kernel::arch::aarch64::uart::Uart;
use crate::kernel::device::registry::DeviceRegistry;
use crate::kernel::kernel::BootInfo;
use crate::kernel::memory::memory_map::{MemoryMap, MemoryRegion};
use crate::main;
use core::arch::asm;

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
        KERNEL.call_once(|| Kernel::new(registry, create_boot_info()));

    main(static_kernel);
}

fn create_boot_info() -> BootInfo {
    BootInfo::new(create_memory_map())
}

fn create_memory_map() -> MemoryMap {
    const FRAME_SIZE: usize = 4096;

    let layout = MemoryLayout::get();

    MemoryMap::new(
        MemoryRegion::new(layout.stack_start, layout.stack_end, FRAME_SIZE),
        MemoryRegion::new(layout.kernel_start, layout.kernel_end, FRAME_SIZE),
        MemoryRegion::new(layout.memory_start, layout.memory_end, FRAME_SIZE),
    )
}
