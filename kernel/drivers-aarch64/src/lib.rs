#![no_std]
#![allow(unsafe_code)]

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

        // SAFETY: символы `__drivers_kernel_start`/`__drivers_kernel_end` объявлены линкером
        // и охватывают единый секционный массив `.drivers.kernel`, заполненный `DriverInfo`-ами
        // через `register_driver!`. `offset_from` корректен: оба указателя из одного объекта,
        // `start <= end` гарантировано раскладкой секции; результат не отрицателен.
        unsafe {
            let start = core::ptr::from_ref(&__drivers_kernel_start);
            let end = core::ptr::from_ref(&__drivers_kernel_end);
            let length = end.offset_from(start).cast_unsigned();
            core::slice::from_raw_parts(start, length)
        }
    }
}

#[macro_export]
macro_rules! register_driver {
    ($symbol:ident, probe = $probe:expr) => {
        #[unsafe(link_section = ".drivers.kernel")]
        #[used]
        static $symbol: drivers_common::DriverInfo<$crate::FdtProbeFn> =
            drivers_common::DriverInfo {
                name: stringify!($symbol),
                probe: $probe as $crate::FdtProbeFn,
            };
    };
}
