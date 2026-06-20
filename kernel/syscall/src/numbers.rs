//! Декодирование сырого syscall-op в kernel-side ошибку.
//!
//! Набор и нумерация операций - часть ABI и живут в [`syscall::SyscallOp`].

pub use syscall::SyscallOp;

use super::error::SyscallError;

/// Декодирует сырой 16-битный op в [`SyscallOp`]; неизвестный код -
/// [`SyscallError::BadSyscall`].
pub const fn op_from_raw(raw: u16) -> Result<SyscallOp, SyscallError> {
    match SyscallOp::from_raw(raw) {
        Some(op) => Ok(op),
        None => Err(SyscallError::BadSyscall),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn op_from_raw_maps_known_and_unknown() {
        assert_eq!(op_from_raw(0x23), Ok(SyscallOp::PortCreate));
        assert_eq!(op_from_raw(0), Err(SyscallError::BadSyscall));
        assert_eq!(op_from_raw(0x21), Err(SyscallError::BadSyscall));
        assert_eq!(op_from_raw(0x7F), Err(SyscallError::BadSyscall));
    }
}
