use crate::kernel::arch::aarch64::uart::UartImpl;
use crate::kernel::device::registry::{DeviceRegistry, UartPtr};
use crate::main;
use core::arch::asm;

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

#[unsafe(no_mangle)]
extern "C" fn early_main() {
    unsafe {
        let registry = create_device_registry();

        // Pass the registry instance to the main
        main(registry);
    }
}

// Create a device registry instance using the DTB parser
unsafe fn create_device_registry() -> DeviceRegistry {
    static mut UART0: UartImpl = UartImpl::new(0x0900_0000);

    let uart_ptr: *mut UartImpl = &raw mut UART0;
    let uart_dyn: UartPtr = uart_ptr as _;

    DeviceRegistry::new(uart_dyn)
}
