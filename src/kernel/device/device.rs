use crate::kernel::core::streams::{InputStream, OutputStream};

pub trait Device {
    fn init(&self);
    fn deinit(&self);
}

pub trait StreamedDevice: OutputStream + InputStream + Device {}

// — пустой маркерный трейт, чтобы можно было работать с &dyn StreamedDevice
impl<T: InputStream + OutputStream + Device> StreamedDevice for T {}
