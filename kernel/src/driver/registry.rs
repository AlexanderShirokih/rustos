use alloc::boxed::Box;
use alloc::vec::Vec;

use fdt::devicetree::{DeviceTree, Node};

use crate::driver::probe::{CompatibleList, NodeProbeExt, ProbeError};

pub trait Device: Send + Sync {
    fn name(&self) -> &str;
}

pub type KernelProbeFn = fn(&Node<'_>) -> Result<Box<dyn Device>, ProbeError>;

#[repr(C)]
pub struct KernelDriverInfo {
    pub name: &'static str,
    pub compatible: CompatibleList,
    pub probe: KernelProbeFn,
}

pub struct DriverRegistry {
    devices: Vec<Box<dyn Device>>,
}

impl Default for DriverRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl<'dt> DriverRegistry {
    pub fn new() -> Self {
        Self {
            devices: Vec::new(),
        }
    }

    pub fn devices(&self) -> &[Box<dyn Device>] {
        &self.devices
    }

    pub fn scan_and_probe(&mut self, dt: &'dt DeviceTree<'dt>) {
        for node in dt.nodes() {
            if node.prop("compatible").is_none() {
                continue;
            }

            if self.try_probe_kernel(&node).is_ok() {
                continue;
            }
        }
    }

    fn try_probe_kernel(&mut self, node: &Node<'dt>) -> Result<(), ProbeError> {
        for driver in kernel_drivers() {
            if node.is_compatible_any(driver.compatible) {
                let device = (driver.probe)(node)?;
                self.devices.push(device);
                return Ok(());
            }
        }

        Err(ProbeError::Unsupported("no kernel driver"))
    }
}

pub fn kernel_drivers() -> &'static [KernelDriverInfo] {
    #[allow(improper_ctypes)]
    unsafe extern "C" {
        static __drivers_kernel_start: KernelDriverInfo;
        static __drivers_kernel_end: KernelDriverInfo;
    }

    unsafe {
        let start = &__drivers_kernel_start as *const KernelDriverInfo;
        let end = &__drivers_kernel_end as *const KernelDriverInfo;
        let len = end.offset_from(start) as usize;
        core::slice::from_raw_parts(start, len)
    }
}

#[macro_export]
macro_rules! register_kernel_driver {
    ($symbol:ident, compatible = $compatible:expr, probe = $probe:expr) => {
        #[unsafe(link_section = ".drivers.kernel")]
        #[used]
        static $symbol: $crate::driver::registry::KernelDriverInfo =
            $crate::driver::registry::KernelDriverInfo {
                name: stringify!($symbol),
                compatible: $compatible,
                probe: $probe,
            };
    };
}
