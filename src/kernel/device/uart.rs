use crate::kernel::core::streams::{InputStream, OutputStream};
use crate::kernel::device::device::Device;
use crate::kernel::device::registry::UartPtr;

pub trait Uart: OutputStream + InputStream + Device {}

/// A wrapper for a UART device that implements OutputStream
pub struct UartOutputStreamWrapper {
    uart_ptr: UartPtr,
}

impl UartOutputStreamWrapper {
    /// Create a new UartOutputStreamWrapper with the given UART pointer
    pub const fn new(uart_ptr: UartPtr) -> Self {
        Self { uart_ptr }
    }
}

impl OutputStream for UartOutputStreamWrapper {
    type WriteError = ();

    fn write(&mut self, byte: u8) -> Result<(), Self::WriteError> {
        unsafe {
            let uart = &mut *self.uart_ptr;
            uart.write(byte)
        }
    }
}
