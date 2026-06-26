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
    /// через прямой вызов `capability`, а не через trap.
    KernelOriginated = 2,
    /// Аргумент syscall'а не прошёл валидацию (например, `HandleId == 0`).
    InvalidArgument = 3,
    /// Невалидный/закрытый handle.
    BadHandle = 4,
    /// Тип объекта не соответствует ожидаемому.
    WrongType = 5,
    /// На handle недостаточно прав.
    AccessDenied = 6,
    /// Операция должна быть повторена позже.
    ShouldWait = 7,
    /// Парный port закрыт.
    PeerClosed = 8,
    /// Истёк deadline.
    Timeout = 9,
    /// Буфер получателя меньше длины сообщения.
    BufferTooSmall = 10,
    /// Сообщение превышает лимит размера.
    MessageTooBig = 11,
    /// Capability-таблица процесса исчерпана.
    OutOfHandles = 12,
    /// Не хватает физической или виртуальной памяти, либо реестр регионов
    /// процесса исчерпан.
    OutOfMemory = 13,
    /// Регион не найден в реестре user-VM текущего процесса.
    NotFound = 14,
    /// Wait отменён: handle, на котором было зарегистрировано ожидание,
    /// был закрыт или передан другому процессу до прихода сигнала.
    Canceled = 15,
    /// Бюджет ресурса исчерпан: метерящая операция запросила больше
    /// страниц, чем осталось в `Resource`.
    ResourceExhausted = 16,
    /// Капа отозвана: предок по цепочке деривации закрыт (или умерла его
    /// таблица). Слот может ещё существовать, но полномочие невалидно.
    Revoked = 17,
}

impl SyscallError {
    #[cfg(test)]
    const ALL: [SyscallError; 17] = [
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
        SyscallError::ResourceExhausted,
        SyscallError::Revoked,
    ];

    pub const fn as_return_value(self) -> i64 {
        -(self as u32 as i64)
    }

    /// Стабильный численный код варианта (часть ABI).
    pub const fn code(self) -> u32 {
        self as u32
    }

    /// Восстанавливает вариант по его коду; `None` на неизвестном коде.
    pub const fn from_code(code: u32) -> Option<Self> {
        match code {
            1 => Some(Self::BadSyscall),
            2 => Some(Self::KernelOriginated),
            3 => Some(Self::InvalidArgument),
            4 => Some(Self::BadHandle),
            5 => Some(Self::WrongType),
            6 => Some(Self::AccessDenied),
            7 => Some(Self::ShouldWait),
            8 => Some(Self::PeerClosed),
            9 => Some(Self::Timeout),
            10 => Some(Self::BufferTooSmall),
            11 => Some(Self::MessageTooBig),
            12 => Some(Self::OutOfHandles),
            13 => Some(Self::OutOfMemory),
            14 => Some(Self::NotFound),
            15 => Some(Self::Canceled),
            16 => Some(Self::ResourceExhausted),
            17 => Some(Self::Revoked),
            _ => None,
        }
    }

    /// Декодирует отрицательный ABI-возврат `-(code as i64)` обратно в вариант;
    /// `None` на неотрицательном значении (успех) или неизвестном коде.
    pub const fn from_return(ret: i64) -> Option<Self> {
        if ret >= 0 || ret < -(u32::MAX as i64) {
            return None;
        }

        Self::from_code(ret.unsigned_abs() as u32)
    }
}

impl From<SyscallError> for i64 {
    fn from(e: SyscallError) -> i64 {
        e.as_return_value()
    }
}

/// Возврат блокирующего Port-syscall "истёк тайм-аут ожидания"
/// (`-(SyscallError::Timeout)`). Возвращается при истечении `timeout_ns`, в
/// т.ч. в режиме poll (`timeout_ns == 0`), когда встречной стороны нет.
pub const SYSCALL_RETURN_TIMEOUT: i64 = -9;

#[cfg(test)]
mod tests {
    extern crate alloc;

    use super::*;

    #[test]
    fn return_constants_match_abi() {
        assert_eq!(
            SyscallError::Timeout.as_return_value(),
            SYSCALL_RETURN_TIMEOUT
        );
    }

    #[test]
    fn return_value_is_negative_and_unique() {
        let codes = SyscallError::ALL;

        for &e in &codes {
            let v: i64 = e.into();
            assert!(v < 0, "{e:?} must encode to a negative value, got {v}");
            assert!(v >= -i64::from(u32::MAX), "{e:?} out of range");
        }
        let seen: alloc::vec::Vec<i64> = codes.iter().map(|&e| e.into()).collect();
        for (i, &v) in seen.iter().enumerate() {
            assert_ne!(v, 0, "{:?} must not encode to success (0)", codes[i]);
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
    fn code_and_from_code_round_trip() {
        for &e in &SyscallError::ALL {
            assert_eq!(SyscallError::from_code(e.code()), Some(e));
            assert_eq!(SyscallError::from_return(e.as_return_value()), Some(e));
        }
        assert_eq!(SyscallError::from_code(0), None);
        assert_eq!(SyscallError::from_code(18), None);
    }

    #[test]
    fn from_return_rejects_success_and_out_of_range() {
        assert_eq!(SyscallError::from_return(0), None);
        assert_eq!(SyscallError::from_return(42), None);
        assert_eq!(SyscallError::from_return(-(i64::from(u32::MAX) + 1)), None);
    }
}
