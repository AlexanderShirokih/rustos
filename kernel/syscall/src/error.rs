//! Кодирование syscall-возврата и преобразования доменных ошибок ядра в
//! [`SyscallError`].

use capability::{IpcError, SpawnError};
use syscall::SyscallError;

/// Преобразует IPC-ошибку в [`SyscallError`].
pub(super) fn map_ipc_error(e: IpcError) -> SyscallError {
    match e {
        IpcError::BadHandle => SyscallError::BadHandle,
        IpcError::WrongType => SyscallError::WrongType,
        IpcError::AccessDenied => SyscallError::AccessDenied,
        IpcError::ShouldWait => SyscallError::ShouldWait,
        IpcError::PeerClosed => SyscallError::PeerClosed,
        IpcError::Timeout => SyscallError::Timeout,
        IpcError::BufferTooSmall => SyscallError::BufferTooSmall,
        IpcError::MessageTooBig => SyscallError::MessageTooBig,
        IpcError::OutOfHandles => SyscallError::OutOfHandles,
        IpcError::Canceled => SyscallError::Canceled,
        IpcError::ResourceExhausted => SyscallError::ResourceExhausted,
        IpcError::Revoked => SyscallError::Revoked,
    }
}

/// Преобразует ошибку запуска процесса в [`SyscallError`].
pub(super) fn map_spawn_error(e: SpawnError) -> SyscallError {
    match e {
        SpawnError::NoFreeProcessSlots
        | SpawnError::NoFreeThreadSlots
        | SpawnError::StackAllocationFailed
        | SpawnError::AddressSpaceCreationFailed => SyscallError::OutOfMemory,
        SpawnError::InvalidName | SpawnError::InvalidPriority | SpawnError::InvalidStackPages => {
            SyscallError::InvalidArgument
        }
        SpawnError::ImageNotLoaded => SyscallError::WrongType,
    }
}

/// Кодирует syscall-результат в формат ABI-возврата (`i64`).
pub(super) fn encode_return(r: Result<u64, SyscallError>) -> i64 {
    match r {
        Ok(v) => i64::try_from(v).unwrap_or(i64::MAX),
        Err(e) => e.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ipc_error_maps_to_syscall_error() {
        assert_eq!(map_ipc_error(IpcError::Canceled), SyscallError::Canceled);
        assert_eq!(map_ipc_error(IpcError::BadHandle), SyscallError::BadHandle);
        assert_eq!(map_ipc_error(IpcError::WrongType), SyscallError::WrongType);
        assert_eq!(
            map_ipc_error(IpcError::AccessDenied),
            SyscallError::AccessDenied
        );
        assert_eq!(
            map_ipc_error(IpcError::ShouldWait),
            SyscallError::ShouldWait
        );
        assert_eq!(
            map_ipc_error(IpcError::PeerClosed),
            SyscallError::PeerClosed
        );
        assert_eq!(map_ipc_error(IpcError::Timeout), SyscallError::Timeout);
        assert_eq!(
            map_ipc_error(IpcError::BufferTooSmall),
            SyscallError::BufferTooSmall
        );
        assert_eq!(
            map_ipc_error(IpcError::MessageTooBig),
            SyscallError::MessageTooBig
        );
        assert_eq!(
            map_ipc_error(IpcError::OutOfHandles),
            SyscallError::OutOfHandles
        );
        assert_eq!(
            map_ipc_error(IpcError::ResourceExhausted),
            SyscallError::ResourceExhausted
        );
        assert_eq!(map_ipc_error(IpcError::Revoked), SyscallError::Revoked);
    }

    #[test]
    fn spawn_error_maps_to_syscall_error() {
        // OOM-семейство.
        for e in [
            SpawnError::NoFreeProcessSlots,
            SpawnError::NoFreeThreadSlots,
            SpawnError::StackAllocationFailed,
            SpawnError::AddressSpaceCreationFailed,
        ] {
            assert_eq!(map_spawn_error(e), SyscallError::OutOfMemory, "{e:?}");
        }
        // Невалидные аргументы.
        for e in [
            SpawnError::InvalidName,
            SpawnError::InvalidPriority,
            SpawnError::InvalidStackPages,
        ] {
            assert_eq!(map_spawn_error(e), SyscallError::InvalidArgument, "{e:?}");
        }
        // Состояние объекта.
        assert_eq!(
            map_spawn_error(SpawnError::ImageNotLoaded),
            SyscallError::WrongType
        );
    }

    #[test]
    fn encode_return_passes_through_success_value() {
        assert_eq!(encode_return(Ok(0)), 0);
        assert_eq!(encode_return(Ok(42)), 42);
    }

    #[test]
    fn encode_return_clamps_oversized_success_to_i64_max() {
        assert_eq!(encode_return(Ok(u64::MAX)), i64::MAX);
        assert_eq!(encode_return(Ok(i64::MAX as u64 + 1)), i64::MAX);
    }

    #[test]
    fn encode_return_encodes_error_as_negative() {
        assert_eq!(
            encode_return(Err(SyscallError::BadHandle)),
            SyscallError::BadHandle.as_return_value()
        );
    }
}
