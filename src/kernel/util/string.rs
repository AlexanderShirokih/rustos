/// Converts an usize value to a string without any dynamic allocation.
/// Returns a string slice to a static buffer.
pub fn usize_to_str(value: usize) -> &'static str {
    // Static buffer to hold the string representation
    // Maximum length of usize (64-bit) in decimal is 20 digits
    static mut BUFFER: [u8; 21] = [0; 21];

    // Safety: This is safe because we ensure exclusive access to the static buffer
    // during the entire function call, and we return a properly bounded slice.
    unsafe {
        // Handle the special case of 0
        if value == 0 {
            BUFFER[0] = b'0';
            return core::str::from_utf8_unchecked(&BUFFER[0..1]);
        }

        // Convert the number to string by repeatedly dividing by 10
        let mut n = value;
        let mut digits = 0;

        // Count the number of digits
        while n > 0 {
            n /= 10;
            digits += 1;
        }

        // Reset n to the original value
        n = value;

        // Fill the buffer from right to left
        for i in (0..digits).rev() {
            BUFFER[i] = b'0' + (n % 10) as u8;
            n /= 10;
        }

        // Return a string slice of the correct length
        core::str::from_utf8_unchecked(&BUFFER[0..digits])
    }
}

/// Converts an usize value to a hexadecimal string without any dynamic allocation.
/// Returns a string slice to a static buffer with "0x" prefix.
pub fn usize_to_hex_str(value: usize) -> &'static str {
    // Static buffer to hold the string representation
    // Maximum length of usize (64-bit) in hex is 16 digits + 2 for "0x" prefix
    static mut BUFFER: [u8; 18] = [0; 18];

    // Safety: This is safe because we ensure exclusive access to the static buffer
    // during the entire function call, and we return a properly bounded slice.
    unsafe {
        // Add "0x" prefix
        BUFFER[0] = b'0';
        BUFFER[1] = b'x';

        // Handle the special case of 0
        if value == 0 {
            BUFFER[2] = b'0';
            return core::str::from_utf8_unchecked(&BUFFER[0..3]);
        }

        // Convert the number to hex string
        let mut n = value;

        // Find the highest non-zero digit position
        let mut temp = value;
        let mut significant_digits = 0;

        while temp > 0 {
            temp >>= 4; // Shift right by 4 bits (divide by 16)
            significant_digits += 1;
        }

        // Fill the buffer from right to left
        for i in (0..significant_digits).rev() {
            let digit = (n & 0xF) as u8; // Get the lowest 4 bits

            // Convert to ASCII
            BUFFER[2 + i] = match digit {
                0..=9 => b'0' + digit,
                10..=15 => b'a' + (digit - 10),
                _ => unreachable!(),
            };

            n >>= 4; // Shift right by 4 bits (divide by 16)
        }

        // Return a string slice of the correct length (prefix + digits)
        core::str::from_utf8_unchecked(&BUFFER[0..(2 + significant_digits)])
    }
}
