//! Номера операций syscall-ABI.
//!
//! 16-битный код, выбранный платформой при входе в trap, отображается
//! в один из вариантов [`SyscallOp`]. Любой неизвестный код отвергается
//! с [`SyscallError::BadSyscall`](super::error::SyscallError::BadSyscall).
//!
//! Стабильность: набор и нумерация - часть ABI и не меняются произвольно;
//! сейчас закреплены три операции, остальные - в roadmap.

use super::error::SyscallError;

/// Закрытый набор поддерживаемых syscall-операций MVP.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u16)]
pub enum SyscallOp {
    ThreadExit = 0,
    ObjectSignal = 1,
    ObjectWaitOne = 2,
}

impl SyscallOp {
    pub const fn from_raw(raw: u16) -> Result<Self, SyscallError> {
        match raw {
            0 => Ok(Self::ThreadExit),
            1 => Ok(Self::ObjectSignal),
            2 => Ok(Self::ObjectWaitOne),
            _ => Err(SyscallError::BadSyscall),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_raw_known_ops() {
        assert_eq!(SyscallOp::from_raw(0), Ok(SyscallOp::ThreadExit));
        assert_eq!(SyscallOp::from_raw(1), Ok(SyscallOp::ObjectSignal));
        assert_eq!(SyscallOp::from_raw(2), Ok(SyscallOp::ObjectWaitOne));
    }

    #[test]
    fn from_raw_unknown_op() {
        assert_eq!(SyscallOp::from_raw(3), Err(SyscallError::BadSyscall));
        assert_eq!(SyscallOp::from_raw(99), Err(SyscallError::BadSyscall));
        assert_eq!(SyscallOp::from_raw(u16::MAX), Err(SyscallError::BadSyscall));
    }
}
