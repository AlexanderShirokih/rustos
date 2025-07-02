use crate::kernel::device::uart::Uart;

// Type alias for the UART trait object pointer
pub type UartPtr = *mut dyn Uart<ReadError = (), WriteError = ()>;

// Instance-based device registry
pub struct DeviceRegistry {
    pub(crate) uart0: UartPtr,
}

impl DeviceRegistry {
    pub fn new(uart0: UartPtr) -> Self {
        Self { uart0 }
    }

    pub fn uart(&self) -> &mut dyn Uart<ReadError = (), WriteError = ()> {
        unsafe { &mut *self.uart0 }
    }
}
