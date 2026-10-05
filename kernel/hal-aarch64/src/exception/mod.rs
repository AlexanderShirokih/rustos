//! Обработка исключений AArch64.

pub(crate) mod esr;
mod exceptions;
pub(crate) mod gpreg;
mod origin;
mod syscall_frame;

pub(crate) use exceptions::ExceptionVectors;
pub(crate) use origin::originates_from_el0;
