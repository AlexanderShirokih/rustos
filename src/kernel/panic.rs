use crate::kernel::device::registry::DeviceRegistry;
use crate::kernel::core::streams::OutputStreamExt;
use core::panic::PanicInfo;
use core::sync::atomic::{AtomicBool, Ordering};

// Global static DeviceRegistry for panic handler
static mut PANIC_REGISTRY: Option<DeviceRegistry> = None;
static PANIC_HANDLER_INITIALIZED: AtomicBool = AtomicBool::new(false);

// Initialize the panic handler with a DeviceRegistry
pub fn init_panic_handler(registry: &DeviceRegistry) {
    if !PANIC_HANDLER_INITIALIZED.load(Ordering::SeqCst) {
        unsafe {
            // Create a copy of the registry
            PANIC_REGISTRY = Some(DeviceRegistry::new(registry.uart0));
            PANIC_HANDLER_INITIALIZED.store(true, Ordering::SeqCst);
        }
    }
}

// Handle a panic by writing information to the UART
#[panic_handler]
pub fn handle_panic(info: &PanicInfo) -> ! {
    // Write a panic message to UART if the registry is initialized
    unsafe {
        if let Some(ref mut registry) = PANIC_REGISTRY {
            let _ = registry.uart().write_str("\n\nPANIC: ");

            // Write panic message if available
            if let Some(location) = info.location() {
                let _ = registry.uart().write_str("at ");
                let _ = registry.uart().write_str(location.file());
                let _ = registry.uart().write_str(":");

                // Convert line number to string manually
                let mut line = location.line();
                let mut digits = [0u8; 10]; // Max 10 digits for u32
                let mut i = 0;

                if line == 0 {
                    digits[0] = b'0';
                    i = 1;
                } else {
                    while line > 0 && i < 10 {
                        digits[i] = (line % 10) as u8 + b'0';
                        line /= 10;
                        i += 1;
                    }
                }

                // Print digits in reverse order
                while i > 0 {
                    i -= 1;
                    let _ = registry.uart().write(digits[i]);
                }
            }

            // Write a simple message
            let message = info.message().as_str().unwrap_or("Unknown panic");
            let _ = registry.uart().write_str("\n\r");
            let _ = registry.uart().write_str(message);
            let _ = registry.uart().write_str("\n\r");
        } else {
            // No registry available, can't output anything
            // Loop forever
        }
    }

    loop {}
}
