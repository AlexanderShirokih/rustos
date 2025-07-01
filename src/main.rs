#![no_std]
#![no_main]

use core::panic::PanicInfo;

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    loop {}
}

#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    // Адрес, куда хотим копировать
    const DEST: *mut u8 = 0x4010_1000 as *mut u8;

    let msg = b"Hello, world!";

    for (i, &b) in msg.iter().enumerate() {
        unsafe {
            core::ptr::write_volatile(DEST.add(i), b);
        }
    }

    loop {
        unsafe {
            core::arch::asm!("wfe");
        }
    }
}
