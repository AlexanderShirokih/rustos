/// Бит "событие наступило". Ядро поднимает его на `IrqLine` при срабатывании линии и на
/// bound-`Signal` процесса либо потока при завершении.
pub const SIGNALED: u32 = 1 << 0;

/// Бит порта "есть что принять": поднят, пока на порту ждёт отправитель либо есть уход значка,
/// который приём ещё не выдал.
pub const READABLE: u32 = 1 << 0;

/// Политика пробуждения `SignalSet`: сколько ждущих будит смена битов.
/// Произвольное N не выражается.
///
/// ABI-коды варианта разрежены: `0`, `1` и `u64::MAX`. Значение вне этих трёх ядро
/// отвергает как `InvalidArgument`. Биты `set` и `clear` применяются при любом варианте.
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
    /// ABI-значение `wake_count` для укладки в аргумент syscall.
    pub const fn to_raw(self) -> u64 {
        match self {
            Self::None => 0,
            Self::One => 1,
            Self::All => u64::MAX,
        }
    }

    /// Восстанавливает вариант по ABI-значению. `None` на любом другом числе.
    pub const fn from_raw(raw: u64) -> Option<Self> {
        match raw {
            0 => Some(Self::None),
            1 => Some(Self::One),
            u64::MAX => Some(Self::All),
            _ => None,
        }
    }
}
