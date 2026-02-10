//! Драйверы устройств AArch64.

#![no_std]

extern crate alloc;

mod commons;
pub mod fdt_adapter;
pub mod tree_ext;

use alloc::boxed::Box;
use drivers_common::{Driver, DriverInfo, EarlyDriver, EarlyDriverInfo, ProbeContext, ProbeResult};
use fdt_adapter::FdtNode;

pub use commons::*;
pub use fdt_adapter::adapt_tree;

pub type FdtProbeContext<'a> = ProbeContext<FdtNode<'a>>;
pub type FdtEarlyProbeFn =
    for<'a> fn(&mut FdtProbeContext<'a>) -> ProbeResult<Box<dyn EarlyDriver>>;
pub type FdtProbeFn = for<'a> fn(&mut FdtProbeContext<'a>) -> ProbeResult<Box<dyn Driver>>;

pub fn early_driver_infos() -> &'static [EarlyDriverInfo<FdtEarlyProbeFn>] {
    {
        #[allow(improper_ctypes)]
        unsafe extern "C" {
            static __drivers_early_start: EarlyDriverInfo<FdtEarlyProbeFn>;
            static __drivers_early_end: EarlyDriverInfo<FdtEarlyProbeFn>;
        }

        unsafe {
            let start = &__drivers_early_start as *const EarlyDriverInfo<FdtEarlyProbeFn>;
            let end = &__drivers_early_end as *const EarlyDriverInfo<FdtEarlyProbeFn>;
            let length = end.offset_from(start) as usize;
            core::slice::from_raw_parts(start, length)
        }
    }
}

pub fn runtime_driver_infos() -> &'static [DriverInfo<FdtProbeFn>] {
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
macro_rules! register_early_driver {
    ($symbol:ident, probe = $probe:expr) => {
        #[cfg_attr(target_os = "none", unsafe(link_section = ".drivers.early"))]
        #[used]
        static $symbol: drivers_common::EarlyDriverInfo<$crate::FdtEarlyProbeFn> =
            drivers_common::EarlyDriverInfo {
                name: stringify!($symbol),
                probe: $probe as $crate::FdtEarlyProbeFn,
            };
    };
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
