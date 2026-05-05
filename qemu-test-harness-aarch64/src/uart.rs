//! PL011 writer для QEMU virt (MMIO 0x0900_0000).

use core::cell::UnsafeCell;

use io::writer::Writer;

#[cfg(all(target_arch = "aarch64", target_os = "none"))]
const PL011_BASE: usize = 0x0900_0000;
#[cfg(all(target_arch = "aarch64", target_os = "none"))]
const UARTDR: usize = 0x000;
#[cfg(all(target_arch = "aarch64", target_os = "none"))]
const UARTFR: usize = 0x018;
#[cfg(all(target_arch = "aarch64", target_os = "none"))]
const UARTFR_TXFF: u32 = 1 << 5;

pub struct Pl011Writer {
    _marker: UnsafeCell<()>,
}

// SAFETY: писатель без состояния; `UnsafeCell<()>` лишь подавляет автоимпл `Sync`.
unsafe impl Send for Pl011Writer {}
// SAFETY: см. `Send`.
unsafe impl Sync for Pl011Writer {}

impl Pl011Writer {
    pub const fn new() -> Self {
        Self {
            _marker: UnsafeCell::new(()),
        }
    }
}

impl Default for Pl011Writer {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(all(target_arch = "aarch64", target_os = "none"))]
fn write_byte(byte: u8) {
    use core::ptr;

    // SAFETY: PL011 в QEMU virt отображён на 0x0900_0000; busy-wait по
    // TXFF предотвращает переполнение FIFO.
    unsafe {
        while ptr::read_volatile((PL011_BASE + UARTFR) as *const u32) & UARTFR_TXFF != 0 {
            core::hint::spin_loop();
        }
        ptr::write_volatile((PL011_BASE + UARTDR) as *mut u32, u32::from(byte));
    }
}

#[cfg(all(target_arch = "aarch64", target_os = "none"))]
impl Writer for Pl011Writer {
    fn write_all(&self, buf: &[u8]) {
        for &b in buf {
            write_byte(b);
        }
    }

    fn flush(&self) {}
}

#[cfg(not(all(target_arch = "aarch64", target_os = "none")))]
impl Writer for Pl011Writer {
    fn write_all(&self, _buf: &[u8]) {}
    fn flush(&self) {}
}
