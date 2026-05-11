//! Ошибки syscall-слоя и кодирование возврата.
//!
//! `SyscallError` расширяет [`IpcError`](kobject::IpcError) тремя
//! syscall-специфичными случаями (`BadSyscall`, `KernelOriginated`,
//! `InvalidArgument`) и переносит остальные `IpcError` 1:1.
//!
//! Возврат syscall кодируется в `i64`: успех - `[0, i64::MAX]`, ошибка -
//! отрицательное значение `-(SyscallError as u32 as i64)` в диапазоне
//! `[-MAX_ERR .. -1]`. Нулевой код для ошибок не используется намеренно,
//! чтобы 0 однозначно означал успех.

use kobject::{IpcError, SpawnError};

/// Ошибки, возвращаемые syscall-слоем.
///
/// Численные коды стабильны и являются частью ABI: они кодируются в
/// возвращаемое значение syscall'а как отрицательные числа.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum SyscallError {
    /// Неизвестный номер операции.
    BadSyscall = 1,
    /// Syscall вызван из kernel-контекста; kernel-side IPC должен ходить
    /// через прямой вызов `kobject`, а не через trap.
    KernelOriginated = 2,
    /// Аргумент syscall'а не прошёл валидацию (например, `HandleId == 0`).
    InvalidArgument = 3,
    /// Невалидный/закрытый handle.
    BadHandle = 4,
    /// Тип объекта не соответствует ожидаемому.
    WrongType = 5,
    /// На handle'е недостаточно прав.
    AccessDenied = 6,
    /// Операция должна быть повторена позже.
    ShouldWait = 7,
    /// Парный endpoint закрыт.
    PeerClosed = 8,
    /// Истёк deadline.
    Timeout = 9,
    /// Буфер получателя меньше длины сообщения.
    BufferTooSmall = 10,
    /// Сообщение превышает лимит размера.
    MessageTooBig = 11,
    /// Handle-таблица процесса исчерпана.
    OutOfHandles = 12,
    /// Не хватает физической или виртуальной памяти, либо реестр регионов
    /// процесса исчерпан.
    OutOfMemory = 13,
    /// Регион не найден в реестре user-VM текущего процесса (например,
    /// `vm_remap` на не-выделенный VA-диапазон).
    NotFound = 14,
    /// Wait отменён: handle, на котором было зарегистрировано ожидание,
    /// был закрыт или передан другому процессу до прихода сигнала.
    Canceled = 15,
}

impl SyscallError {
    /// Кодирование возврата: отрицательное значение `i64`.
    pub const fn as_return_value(self) -> i64 {
        -(self as u32 as i64)
    }
}

impl From<IpcError> for SyscallError {
    fn from(e: IpcError) -> Self {
        match e {
            IpcError::BadHandle => Self::BadHandle,
            IpcError::WrongType => Self::WrongType,
            IpcError::AccessDenied => Self::AccessDenied,
            IpcError::ShouldWait => Self::ShouldWait,
            IpcError::PeerClosed => Self::PeerClosed,
            IpcError::Timeout => Self::Timeout,
            IpcError::BufferTooSmall => Self::BufferTooSmall,
            IpcError::MessageTooBig => Self::MessageTooBig,
            IpcError::OutOfHandles => Self::OutOfHandles,
            IpcError::Canceled => Self::Canceled,
        }
    }
}

impl From<SpawnError> for SyscallError {
    fn from(e: SpawnError) -> Self {
        match e {
            SpawnError::NoFreeProcessSlots
            | SpawnError::NoFreeThreadSlots
            | SpawnError::StackAllocationFailed
            | SpawnError::AddressSpaceCreationFailed => Self::OutOfMemory,
            SpawnError::InvalidPriority | SpawnError::InvalidStackPages => Self::InvalidArgument,
            SpawnError::ImageNotLoaded => Self::WrongType,
        }
    }
}

impl From<SyscallError> for i64 {
    fn from(e: SyscallError) -> i64 {
        e.as_return_value()
    }
}

/// Кодирует syscall-результат в формат ABI-возврата (`i64`).
pub(super) fn encode_return(r: Result<u64, SyscallError>) -> i64 {
    match r {
        // Успех - маска сигналов в u32 либо 0, влезает в i64 без
        // потери знака.
        Ok(v) => i64::try_from(v).unwrap_or(i64::MAX),
        Err(e) => e.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ipc_error_maps_to_syscall_error() {
        assert_eq!(
            SyscallError::from(IpcError::Canceled),
            SyscallError::Canceled
        );
        assert_eq!(
            SyscallError::from(IpcError::BadHandle),
            SyscallError::BadHandle
        );
        assert_eq!(
            SyscallError::from(IpcError::WrongType),
            SyscallError::WrongType
        );
        assert_eq!(
            SyscallError::from(IpcError::AccessDenied),
            SyscallError::AccessDenied
        );
        assert_eq!(
            SyscallError::from(IpcError::ShouldWait),
            SyscallError::ShouldWait
        );
        assert_eq!(
            SyscallError::from(IpcError::PeerClosed),
            SyscallError::PeerClosed
        );
        assert_eq!(SyscallError::from(IpcError::Timeout), SyscallError::Timeout);
        assert_eq!(
            SyscallError::from(IpcError::BufferTooSmall),
            SyscallError::BufferTooSmall
        );
        assert_eq!(
            SyscallError::from(IpcError::MessageTooBig),
            SyscallError::MessageTooBig
        );
        assert_eq!(
            SyscallError::from(IpcError::OutOfHandles),
            SyscallError::OutOfHandles
        );
    }

    #[test]
    fn return_value_is_negative_and_unique() {
        let codes = [
            SyscallError::BadSyscall,
            SyscallError::KernelOriginated,
            SyscallError::InvalidArgument,
            SyscallError::BadHandle,
            SyscallError::WrongType,
            SyscallError::AccessDenied,
            SyscallError::ShouldWait,
            SyscallError::PeerClosed,
            SyscallError::Timeout,
            SyscallError::BufferTooSmall,
            SyscallError::MessageTooBig,
            SyscallError::OutOfHandles,
            SyscallError::OutOfMemory,
            SyscallError::NotFound,
            SyscallError::Canceled,
        ];
        for &e in &codes {
            let v: i64 = e.into();
            assert!(v < 0, "{e:?} must encode to a negative value, got {v}");
            assert!(v >= -i64::from(u32::MAX), "{e:?} out of range");
        }
        // Все коды разные.
        let mut seen = [0_i64; 15];
        for (i, &e) in codes.iter().enumerate() {
            seen[i] = e.into();
        }
        for i in 0..codes.len() {
            for j in (i + 1)..codes.len() {
                assert_ne!(
                    seen[i], seen[j],
                    "duplicate code: {:?}/{:?}",
                    codes[i], codes[j]
                );
            }
        }
    }

    #[test]
    fn return_value_zero_means_success_only() {
        // Никакой код ошибки не должен мапиться в 0.
        let codes = [
            SyscallError::BadSyscall,
            SyscallError::KernelOriginated,
            SyscallError::InvalidArgument,
            SyscallError::BadHandle,
            SyscallError::WrongType,
            SyscallError::AccessDenied,
            SyscallError::ShouldWait,
            SyscallError::PeerClosed,
            SyscallError::Timeout,
            SyscallError::BufferTooSmall,
            SyscallError::MessageTooBig,
            SyscallError::OutOfHandles,
            SyscallError::OutOfMemory,
            SyscallError::NotFound,
            SyscallError::Canceled,
        ];
        for &e in &codes {
            assert_ne!(i64::from(e), 0);
        }
    }
}
