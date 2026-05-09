//! Номера операций syscall-ABI.
//!
//! 16-битный код, выбранный платформой при входе в trap, отображается
//! в один из вариантов [`SyscallOp`]. Любой неизвестный код отвергается
//! с [`SyscallError::BadSyscall`](super::error::SyscallError::BadSyscall).
//!
//! # Группировка
//!
//! Номера группируются по высокому ниблу: операции одного класса лежат
//! в общем диапазоне, что упрощает добавление новых вариантов и оставляет
//! место под будущие миграции.
//!
//! | Диапазон      | Класс операций                              |
//! |---------------|---------------------------------------------|
//! | `0x00`        | резерв (`raw == 0` -> `BadSyscall`)          |
//! | `0x01..=0x0F` | thread/process control                      |
//! | `0x10..=0x1F` | object base - общие операции на любом KO    |
//! | `0x20..=0x2F` | channel-специфичные                         |
//! | `0x30..=0x3F` | handle lifecycle                            |
//! | `0x40..=0x4F` | резерв под Process KObject                  |
//! | `0x50..=0x5F` | резерв под Thread KObject                   |
//! | `0x60..=0x6F` | резерв под Memory KObject                   |
//! | `0x70..=0x7F` | резерв под Port KObject                     |
//!
//! `ChannelWrite`/`ChannelRead` (передача [`Message`](kobject::Message)
//! через регистры невозможна - нужен user-pointer protocol) приедут в
//! диапазон `0x20..=0x2F` после введения соответствующего ABI; здесь
//! их пока нет.
//!
//! Стабильность: набор и нумерация - часть ABI и не меняются произвольно.

use super::error::SyscallError;

/// Закрытый набор поддерживаемых syscall-операций.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u16)]
pub enum SyscallOp {
    // 0x01..=0x0F - thread/process control.
    ThreadExit = 0x01,

    // 0x10..=0x1F - object base.
    ObjectSignal = 0x10,
    /// Ждёт сигналы KO. Аргументы: `arg0=handle`, `arg1=signals`
    /// (нижние 32 бита), `arg2=timeout_ns`; `timeout_ns == 0` -
    /// non-blocking poll, ненулевое значение - относительный timeout.
    ObjectWaitOne = 0x11,

    // 0x20..=0x2F - channel.
    ChannelCreate = 0x20,

    // 0x30..=0x3F - handle lifecycle.
    HandleClose = 0x30,
    HandleDuplicate = 0x31,

    // Memory: per-process user-VM management.
    /// Выделяет user-память: маппит свежие фреймы в свободный VA процесса
    /// и возвращает базовый адрес. Аргументы: `arg0=size_bytes`,
    /// `arg1=flags_raw` (см. [`UserMemFlags`](crate::UserMemFlags)).
    MemoryAllocate = 0x60,
    /// Меняет флаги уже выделенного региона (аналог `MemoryMapper::remap`).
    /// Аргументы: `arg0=va`, `arg1=size_bytes`, `arg2=flags_raw`.
    MemoryRemap = 0x61,
}

impl SyscallOp {
    pub const fn from_raw(raw: u16) -> Result<Self, SyscallError> {
        match raw {
            0x01 => Ok(Self::ThreadExit),
            0x10 => Ok(Self::ObjectSignal),
            0x11 => Ok(Self::ObjectWaitOne),
            0x20 => Ok(Self::ChannelCreate),
            0x30 => Ok(Self::HandleClose),
            0x31 => Ok(Self::HandleDuplicate),
            0x60 => Ok(Self::MemoryAllocate),
            0x61 => Ok(Self::MemoryRemap),
            _ => Err(SyscallError::BadSyscall),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_raw_known_ops() {
        assert_eq!(SyscallOp::from_raw(0x01), Ok(SyscallOp::ThreadExit));
        assert_eq!(SyscallOp::from_raw(0x10), Ok(SyscallOp::ObjectSignal));
        assert_eq!(SyscallOp::from_raw(0x11), Ok(SyscallOp::ObjectWaitOne));
        assert_eq!(SyscallOp::from_raw(0x20), Ok(SyscallOp::ChannelCreate));
        assert_eq!(SyscallOp::from_raw(0x30), Ok(SyscallOp::HandleClose));
        assert_eq!(SyscallOp::from_raw(0x31), Ok(SyscallOp::HandleDuplicate));
        assert_eq!(SyscallOp::from_raw(0x60), Ok(SyscallOp::MemoryAllocate));
        assert_eq!(SyscallOp::from_raw(0x61), Ok(SyscallOp::MemoryRemap));
    }

    #[test]
    fn from_raw_zero_is_bad_syscall() {
        // 0x00 зарезервирован: трапы с обнулённым op-регистром не должны
        // случайно попадать в реальную операцию.
        assert_eq!(SyscallOp::from_raw(0), Err(SyscallError::BadSyscall));
    }

    #[test]
    fn from_raw_unknown_op() {
        assert_eq!(SyscallOp::from_raw(3), Err(SyscallError::BadSyscall));
        assert_eq!(SyscallOp::from_raw(0x12), Err(SyscallError::BadSyscall));
        assert_eq!(SyscallOp::from_raw(0x32), Err(SyscallError::BadSyscall));
        assert_eq!(SyscallOp::from_raw(0x40), Err(SyscallError::BadSyscall));
        assert_eq!(SyscallOp::from_raw(99), Err(SyscallError::BadSyscall));
        assert_eq!(SyscallOp::from_raw(u16::MAX), Err(SyscallError::BadSyscall));
    }
}
