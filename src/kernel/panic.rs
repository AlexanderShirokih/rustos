use core::panic::PanicInfo;

// Handle a panic by writing information to the UART
#[panic_handler]
pub fn handle_panic(info: &PanicInfo) -> ! {
    crate::log::print("\n\rPANIC: ");

    // Write panic message if available
    if let Some(location) = info.location() {
        crate::log::print("at ");
        crate::log::print(location.file());
        crate::log::print(":");

        // Convert line number to string manually
        let mut line = location.line();
        let mut digits = [0u8; 10]; // Max 10 digits for u32
        let mut i = 0;

        if line == 0 {
            digits[0] = 0;
            i = 1;
        } else {
            while line > 0 && i < 10 {
                digits[i] = (line % 10) as u8;
                line /= 10;
                i += 1;
            }
        }

        let digits_map = ["0", "1", "2", "3", "4", "5", "6", "7", "8", "9"];
        // Print digits in reverse order
        while i > 0 {
            i -= 1;
            crate::log::print(digits_map[digits[i] as usize]);
        }
    }

    // Write a simple message
    let message = info.message().as_str().unwrap_or("Unknown panic");
    crate::log::print("\n\r");
    crate::log::print(message);
    crate::log::print("\n\r");

    loop {}
}
