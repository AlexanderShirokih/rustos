//! Кодировка прав доступа к user-памяти, передаваемых из user-кода через
//! syscall-аргумент. ABI: 0 = RW, 1 = RO, 2 = RX. Неизвестное значение -
//! ошибка валидации, маппится в `InvalidArgument` соответствующим syscall-ом.

use memory::MemFlags;

/// Закодированные пользовательскими программами права доступа к памяти.
///
/// Стабильны и являются частью ABI: значение передаётся как `u64`
/// аргумент syscall'а.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum UserMemFlags {
    /// User read/write, без исполнения.
    ReadWrite = 0,
    /// User read-only, без исполнения.
    ReadOnly = 1,
    /// User read + execute (для исполняемых сегментов user-кода).
    ReadExecute = 2,
}

/// Ошибка декодирования сырого `u64`-аргумента в [`UserMemFlags`].
///
/// Локальный тип, не зависящий от syscall-слоя: вызывающий сам мапит его в
/// свой error-domain (например, `SyscallError::InvalidArgument`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InvalidUserMemFlags;

impl UserMemFlags {
    pub const fn from_raw(raw: u64) -> Result<Self, InvalidUserMemFlags> {
        match raw {
            0 => Ok(Self::ReadWrite),
            1 => Ok(Self::ReadOnly),
            2 => Ok(Self::ReadExecute),
            _ => Err(InvalidUserMemFlags),
        }
    }

    pub const fn to_mem_flags(self) -> MemFlags {
        match self {
            Self::ReadWrite => MemFlags::user_rw(),
            Self::ReadOnly => MemFlags::user_ro(),
            Self::ReadExecute => MemFlags::user_rx(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_mem_flags_from_raw_known() {
        assert_eq!(UserMemFlags::from_raw(0), Ok(UserMemFlags::ReadWrite));
        assert_eq!(UserMemFlags::from_raw(1), Ok(UserMemFlags::ReadOnly));
        assert_eq!(UserMemFlags::from_raw(2), Ok(UserMemFlags::ReadExecute));
    }

    #[test]
    fn user_mem_flags_from_raw_unknown_is_invalid() {
        assert_eq!(UserMemFlags::from_raw(3), Err(InvalidUserMemFlags));
        assert_eq!(UserMemFlags::from_raw(u64::MAX), Err(InvalidUserMemFlags));
    }

    #[test]
    fn user_mem_flags_round_trip_to_mem_flags() {
        // Просто smoke: преобразование не должно паниковать; более
        // подробные проверки прав - в `MemFlags::user_*` юнитах.
        let _ = UserMemFlags::ReadWrite.to_mem_flags();
        let _ = UserMemFlags::ReadOnly.to_mem_flags();
        let _ = UserMemFlags::ReadExecute.to_mem_flags();
    }
}
