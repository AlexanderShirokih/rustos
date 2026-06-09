//! Декодирование сырого syscall-op в kernel-side ошибку.
//!
//! Набор и нумерация операций - часть ABI и живут в [`userland_abi::SyscallOp`].

pub use userland_abi::SyscallOp;

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
        assert_eq!(op_from_raw(0x21), Ok(SyscallOp::ChannelWrite));
        assert_eq!(op_from_raw(0), Err(SyscallError::BadSyscall));
        assert_eq!(op_from_raw(0x7F), Err(SyscallError::BadSyscall));
    }

    #[test]
    fn abi_mirror_channel_signal_peer_closed_matches_kobject() {
        assert_eq!(
            userland_abi::CHANNEL_SIGNAL_PEER_CLOSED,
            kobject::CHANNEL_PEER_CLOSED
        );
    }

    #[test]
    fn abi_mirror_syscall_return_timeout_matches_error_encoding() {
        assert_eq!(
            userland_abi::SYSCALL_RETURN_TIMEOUT,
            SyscallError::Timeout.as_return_value()
        );
    }
}
