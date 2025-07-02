use crate::kernel::core::streams::{InputStream, OutputStream};
use crate::kernel::device::device::Device;
use crate::kernel::device::uart::Uart;

pub struct UartImpl {
    base_address: *mut u32,
}

impl UartImpl {
    pub const fn new(base_address: usize) -> Self {
        Self {
            base_address: base_address as *mut u32,
        }
    }
}

impl OutputStream for UartImpl {
    type WriteError = ();

    fn write(&mut self, byte: u8) -> Result<(), Self::WriteError> {
        unsafe {
            core::ptr::write_volatile(self.base_address, byte as u32);

            Ok(())
        }
    }
}

impl InputStream for UartImpl {
    type ReadError = ();

    fn read(&mut self) -> Result<u8, Self::ReadError> {
        let byte = unsafe { core::ptr::read_volatile(self.base_address) } as u8;
        Ok(byte)
    }
}

impl<T: Uart> Device for T {
    fn init(&mut self) {
        // we think it will be enabled from the start
    }

    fn deinit(&mut self) {
        // disable device register?
    }
}

impl Uart for UartImpl {}
