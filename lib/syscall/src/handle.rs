use core::num::NonZeroU32;

use crate::SyscallError;

/// Сырой HandleId syscall-ABI: 32-битный индекс записи в handle-таблице, где
/// `0` означает невалидный handle.
pub type RawHandle = u32;

/// Capability процесса: непрозрачный идентификатор записи в handle-таблице.
#[repr(transparent)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Handle(NonZeroU32);

impl Handle {
    /// Оборачивает сырой HandleId; `None` при нулевом значении.
    pub const fn new(raw: RawHandle) -> Option<Self> {
        match NonZeroU32::new(raw) {
            Some(nz) => Some(Self(nz)),
            None => None,
        }
    }

    /// Сырое 32-битное значение.
    pub const fn raw(self) -> u32 {
        self.0.get()
    }

    /// Разбирает знаковый возврат системного вызова:
    /// значение в `1..=u32::MAX` - валидный handle,
    /// иначе декодированный `SyscallError`.
    pub const fn from_syscall_return(ret: i64) -> Result<Self, SyscallError> {
        if ret >= 1 && ret <= u32::MAX as i64 {
            #[allow(clippy::cast_sign_loss)]
            let handle = ret as u32;
            match NonZeroU32::new(handle) {
                Some(nz) => Ok(Self(nz)),
                None => Err(SyscallError::BadSyscall),
            }
        } else {
            match SyscallError::from_return(ret) {
                Some(e) => Err(e),
                None => Err(SyscallError::BadSyscall),
            }
        }
    }
}

/// Запись массива `SignalWaitMany`: capability target `handle` и маска ожидаемых сигналов
/// `mask`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(C)]
pub struct WaitItem {
    handle: Handle,
    mask: u32,
}

impl WaitItem {
    /// Собирает запись из `handle` и ненулевой маски сигналов `mask`;
    /// нулевую маску ядро отвергает как `InvalidArgument`.
    pub const fn new(handle: Handle, mask: u32) -> Self {
        Self { handle, mask }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn handle_new_rejects_zero() {
        assert!(Handle::new(0).is_none());
        assert_eq!(Handle::new(7).map(Handle::raw), Some(7));
    }

    #[test]
    fn handle_from_syscall_return_splits_sign() {
        assert_eq!(Handle::from_syscall_return(5).map(Handle::raw), Ok(5));
        assert_eq!(
            Handle::from_syscall_return(-7),
            Err(SyscallError::ShouldWait)
        );
        assert_eq!(
            Handle::from_syscall_return(0),
            Err(SyscallError::BadSyscall)
        );
        assert_eq!(
            Handle::from_syscall_return(i64::from(u32::MAX) + 1),
            Err(SyscallError::BadSyscall)
        );
    }

    #[test]
    fn wait_item_wire_layout() {
        assert_eq!(size_of::<WaitItem>(), 8);
        assert_eq!(align_of::<WaitItem>(), 4);
        assert_eq!(core::mem::offset_of!(WaitItem, handle), 0);
        assert_eq!(core::mem::offset_of!(WaitItem, mask), 4);
    }
}
