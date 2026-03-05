#![no_std]

extern crate alloc;

use drivers_common::{
    DriverInfo,
    probe::{ProbeContext, ProbeResult},
};
use drivers_common_aarch64::fdt_adapter::FdtNode;

pub mod generic;
pub mod qcom;
mod sysreg;

pub type FdtProbeContext<'a> = ProbeContext<FdtNode<'a>>;
pub type FdtProbeFn = for<'a> fn(&mut FdtProbeContext<'a>) -> ProbeResult;

pub fn drivers() -> &'static [DriverInfo<FdtProbeFn>] {
    {
        #[allow(improper_ctypes)]
        unsafe extern "C" {
            static __drivers_kernel_start: DriverInfo<FdtProbeFn>;
            static __drivers_kernel_end: DriverInfo<FdtProbeFn>;
        }

        unsafe {
            let start = &__drivers_kernel_start as *const DriverInfo<FdtProbeFn>;
            let end = &__drivers_kernel_end as *const DriverInfo<FdtProbeFn>;
            let length = end.offset_from(start) as usize;
            core::slice::from_raw_parts(start, length)
        }
    }
}

#[macro_export]
macro_rules! register_driver {
    ($symbol:ident, probe = $probe:expr) => {
        #[cfg_attr(target_os = "none", unsafe(link_section = ".drivers.kernel"))]
        #[used]
        static $symbol: drivers_common::DriverInfo<$crate::FdtProbeFn> =
            drivers_common::DriverInfo {
                name: stringify!($symbol),
                probe: $probe as $crate::FdtProbeFn,
            };
    };
}
