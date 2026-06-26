/// Бит сигнала `Signal` "событие наступило".
pub const SIGNALED: u32 = 1 << 0;

/// Политика пробуждения `SignalSet`: сколько ждущих будит смена битов.
/// Произвольное N не выражается.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WakeCount {
    /// Только сменить биты, никого не будить (clear бита, взвод sticky-бита).
    None,
    /// Разбудить ровно одного ждущего в FIFO-порядке.
    One,
    /// Разбудить всех пересекающихся ждущих.
    All,
}

impl WakeCount {
    pub const fn to_raw(self) -> u64 {
        match self {
            Self::None => 0,
            Self::One => 1,
            Self::All => u64::MAX,
        }
    }

    pub const fn from_raw(raw: u64) -> Option<Self> {
        match raw {
            0 => Some(Self::None),
            1 => Some(Self::One),
            u64::MAX => Some(Self::All),
            _ => None,
        }
    }
}
