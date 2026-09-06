//! Собирает драйверы устройств AArch64 в секцию `.drivers.kernel` и отдаёт их список ядру.
//!
//! [`register_driver!`] кладёт дескриптор драйвера в эту секцию на этапе компоновки.
//! [`drivers`] отдаёт список, который линкер собрал из этой секции. По этому списку
//! `kernelspace` ведёт проходы подъёма.
//!
//! Здесь лежат драйверы generic-периферии ARM - таймер, GICv2, GICv3, UART PL011 - и UART DM
//! платформы Qualcomm. Только этот из крейтов драйверов обращается к регистрам устройств.
//! `drivers-common` и `drivers-common-aarch64` разбирают описания устройств.

#![no_std]
#![allow(unsafe_code)]
#![allow(
    clippy::cast_possible_truncation,
    clippy::indexing_slicing,
    clippy::multiple_unsafe_ops_per_block
)]

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
        // через `register_driver!`. Оба указателя происходят из одного объекта. Раскладка
        // секции гарантирует `start <= end`, поэтому `offset_from` корректен и результат не
        // отрицателен.
        unsafe {
            let start = core::ptr::from_ref(&__drivers_kernel_start);
            let end = core::ptr::from_ref(&__drivers_kernel_end);
            let length = end.offset_from(start).cast_unsigned();
            core::slice::from_raw_parts(start, length)
        }
    }
}

/// Порождает статик `DriverInfo<FdtProbeFn>` в секции `.drivers.kernel`.
///
/// Имя записи берётся из `stringify!($symbol)`. Поле `probe` хранит выражение `probe`,
/// приведённое к `FdtProbeFn`. Срез между границами секции собирает [`drivers`].
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
