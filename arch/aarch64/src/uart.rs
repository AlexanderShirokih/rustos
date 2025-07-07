use kernel_core::device::device::Device;
use kernel_core::streams::{InputStream, OutputStream};
use spin::Mutex;

pub struct Uart {
    base_address: *mut u32,
    lock: Mutex<()>,
}

unsafe impl Send for Uart {}
unsafe impl Sync for Uart {}

impl Uart {
    pub const fn new(base_address: usize) -> Self {
        Self {
            base_address: base_address as *mut u32,
            lock: Mutex::new(()),
        }
    }
}

impl OutputStream for Uart {
    type WriteError = ();

    fn write(&self, byte: u8) -> Result<(), Self::WriteError> {
        let _guard = self.lock.lock();

        unsafe {
            core::ptr::write_volatile(self.base_address, byte as u32);
        }

        Ok(())
    }
}

impl InputStream for Uart {
    type ReadError = ();

    fn read(&self) -> Result<u8, Self::ReadError> {
        let byte = unsafe { core::ptr::read_volatile(self.base_address) } as u8;
        Ok(byte)
    }
}

impl Device for Uart {
    fn init(&self) {
        // we think it will be enabled from the start
    }

    fn deinit(&self) {
        // disable device register?
    }
}
