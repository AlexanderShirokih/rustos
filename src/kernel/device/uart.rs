use crate::kernel::core::streams::{InputStream, OutputStream};
use crate::kernel::device::device::Device;

pub trait Uart: OutputStream + InputStream + Device {}
