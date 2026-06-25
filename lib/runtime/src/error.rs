//! Нормализация сырых syscall-возвратов в `Result`.

use syscall::SyscallError;

/// Ошибка рантайма поверх сырого syscall-возврата.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// Распознанный код ошибки syscall-слоя.
    Syscall(SyscallError),
    /// Отрицательный возврат с неизвестным кодом: баг декодирования или
    /// version-skew; не сводится к рабочей ошибке.
    Unknown(i64),
}

/// Результат операции рантайма.
pub type Result<T> = core::result::Result<T, Error>;

impl Error {
    /// Декодирует отрицательный возврат: известный код - `Syscall`, иначе
    /// `Unknown`.
    pub fn from_return(ret: i64) -> Self {
        match SyscallError::from_return(ret) {
            Some(error) => Self::Syscall(error),
            None => Self::Unknown(ret),
        }
    }
}

/// Разбирает возврат-код: `0` - успех, отрицательное - ошибка.
pub(crate) fn unit(ret: i64) -> Result<()> {
    if ret == 0 {
        Ok(())
    } else {
        Err(Error::from_return(ret))
    }
}
