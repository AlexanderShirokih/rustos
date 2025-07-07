use crate::device::device::StreamedDevice;

pub struct DeviceRegistry {
    uart0: &'static (dyn StreamedDevice<ReadError = (), WriteError = ()> + Sync),
}

impl DeviceRegistry {
    pub fn new(
        uart0: &'static (dyn StreamedDevice<ReadError = (), WriteError = ()> + Sync),
    ) -> Self {
        Self { uart0 }
    }

    pub fn uart(&self) -> &'static (dyn StreamedDevice<ReadError = (), WriteError = ()> + Sync) {
        self.uart0
    }
}
