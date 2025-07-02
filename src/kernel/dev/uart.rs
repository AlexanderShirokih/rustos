use crate::kernel::core::streams::{InputStream, OutputStream};
use crate::kernel::dev::device::Device;

pub trait Uart: OutputStream + InputStream + Device {}
