use core::ops::{BitAnd, BitAndAssign, BitOr, BitOrAssign, Not};

/// Набор прав, ассоциированных с конкретным [`Handle`](super::Handle).
///
/// Хранится отдельно от объекта: один и тот же KO может быть доступен
/// разным процессам через handle'ы с разными правами.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
#[repr(transparent)]
pub struct Rights(u32);

impl Rights {
    pub const NONE: Self = Self(0);
    pub const DUPLICATE: Self = Self(1 << 0);
    pub const TRANSFER: Self = Self(1 << 1);
    pub const READ: Self = Self(1 << 2);
    pub const WRITE: Self = Self(1 << 3);
    pub const SIGNAL: Self = Self(1 << 4);
    pub const WAIT: Self = Self(1 << 5);
    pub const INSPECT: Self = Self(1 << 6);
    pub const MANAGE_THREAD: Self = Self(1 << 7);
    pub const MANAGE_PROCESS: Self = Self(1 << 8);

    const ALL_BITS: u32 = Self::DUPLICATE.0
        | Self::TRANSFER.0
        | Self::READ.0
        | Self::WRITE.0
        | Self::SIGNAL.0
        | Self::WAIT.0
        | Self::INSPECT.0
        | Self::MANAGE_THREAD.0
        | Self::MANAGE_PROCESS.0;

    pub const fn empty() -> Self {
        Self(0)
    }

    pub const fn all() -> Self {
        Self(Self::ALL_BITS)
    }

    pub const fn bits(self) -> u32 {
        self.0
    }

    /// Принимает произвольный набор битов, отбрасывая неизвестные позиции.
    pub const fn from_bits_truncate(bits: u32) -> Self {
        Self(bits & Self::ALL_BITS)
    }

    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// `true`, если `self` содержит все биты из `other`.
    pub const fn contains(self, other: Self) -> bool {
        (self.0 & other.0) == other.0
    }

    /// `true`, если все биты `self` уже присутствуют в `other`.
    pub const fn is_subset_of(self, other: Self) -> bool {
        (self.0 & other.0) == self.0
    }

    /// Стартовый набор прав для свежесозданного KO данного типа.
    pub fn defaults_for(obj: &super::object::KObject) -> Self {
        use super::object::KObject;

        const SIGNALABLE: u32 = Rights::SIGNAL.0
            | Rights::WAIT.0
            | Rights::DUPLICATE.0
            | Rights::TRANSFER.0
            | Rights::INSPECT.0;

        match obj {
            KObject::Channel(_) => Self(
                Self::READ.0
                    | Self::WRITE.0
                    | Self::WAIT.0
                    | Self::TRANSFER.0
                    | Self::DUPLICATE.0
                    | Self::INSPECT.0,
            ),
            KObject::Event(_) => Self(SIGNALABLE),
            // SIGNAL не выдаём: PROCESS_TERMINATED поднимает только ядро.
            KObject::Process(_) => Self(
                Self::WAIT.0
                    | Self::INSPECT.0
                    | Self::MANAGE_PROCESS.0
                    | Self::DUPLICATE.0
                    | Self::TRANSFER.0,
            ),
            // SIGNAL не выдаём: THREAD_TERMINATED поднимает только ядро.
            KObject::Thread(_) => Self(
                Self::WAIT.0
                    | Self::INSPECT.0
                    | Self::MANAGE_THREAD.0
                    | Self::DUPLICATE.0
                    | Self::TRANSFER.0,
            ),
        }
    }
}

impl BitOr for Rights {
    type Output = Self;
    fn bitor(self, rhs: Self) -> Self {
        Self(self.0 | rhs.0)
    }
}

impl BitOrAssign for Rights {
    fn bitor_assign(&mut self, rhs: Self) {
        self.0 |= rhs.0;
    }
}

impl BitAnd for Rights {
    type Output = Self;
    fn bitand(self, rhs: Self) -> Self {
        Self(self.0 & rhs.0)
    }
}

impl BitAndAssign for Rights {
    fn bitand_assign(&mut self, rhs: Self) {
        self.0 &= rhs.0;
    }
}

impl Not for Rights {
    type Output = Self;
    fn not(self) -> Self {
        Self(!self.0 & Self::ALL_BITS)
    }
}

#[cfg(test)]
mod tests {
    use super::{
        super::{object::KObject, process::ProcessObject, thread::ThreadObject},
        *,
    };

    #[test]
    fn defaults_for_process_grants_manage_and_wait() {
        let ko = KObject::Process(ProcessObject::new());
        let r = Rights::defaults_for(&ko);
        assert!(r.contains(Rights::MANAGE_PROCESS));
        assert!(r.contains(Rights::WAIT));
        assert!(r.contains(Rights::INSPECT));
        assert!(r.contains(Rights::DUPLICATE));
        assert!(r.contains(Rights::TRANSFER));
    }

    #[test]
    fn defaults_for_process_omits_signal_and_io() {
        let ko = KObject::Process(ProcessObject::new());
        let r = Rights::defaults_for(&ko);
        assert!(!r.contains(Rights::SIGNAL));
        assert!(!r.contains(Rights::READ));
        assert!(!r.contains(Rights::WRITE));
        assert!(!r.contains(Rights::MANAGE_THREAD));
    }

    #[test]
    fn defaults_for_thread_grants_manage_and_wait() {
        let ko = KObject::Thread(ThreadObject::new());
        let r = Rights::defaults_for(&ko);
        assert!(r.contains(Rights::MANAGE_THREAD));
        assert!(r.contains(Rights::WAIT));
        assert!(r.contains(Rights::INSPECT));
        assert!(r.contains(Rights::DUPLICATE));
        assert!(r.contains(Rights::TRANSFER));
    }

    #[test]
    fn defaults_for_thread_omits_signal_and_io() {
        let ko = KObject::Thread(ThreadObject::new());
        let r = Rights::defaults_for(&ko);
        assert!(!r.contains(Rights::SIGNAL));
        assert!(!r.contains(Rights::READ));
        assert!(!r.contains(Rights::WRITE));
        assert!(!r.contains(Rights::MANAGE_PROCESS));
    }
}
