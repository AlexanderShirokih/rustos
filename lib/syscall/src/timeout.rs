const NS_PER_SEC: u64 = 1_000_000_000;
const NS_PER_MILLI: u64 = 1_000_000;
const NS_PER_MICRO: u64 = 1_000;

/// Типизированный фасад над `timeout_ns` блокирующих syscall. Значение отсчитывается от
/// момента вызова, два sentinel-значения выделены под poll и бессрочное ожидание. Порядок
/// сроков совпадает с порядком их длительности: [`Self::POLL`] короче любого срока,
/// [`Self::INFINITE`] длиннее любого.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[repr(transparent)]
pub struct Timeout(u64);

impl Timeout {
    /// Операция завершается немедленно. Без готовой встречной стороны возврат -
    /// [`crate::SYSCALL_RETURN_TIMEOUT`].
    pub const POLL: Self = Self(0);

    /// Ждать бессрочно (не занимает слот в sleeper-heap).
    pub const INFINITE: Self = Self(u64::MAX);

    /// Относительный дедлайн в наносекундах. `u64::MAX` совпадает с [`Self::INFINITE`],
    /// `0` - с [`Self::POLL`].
    pub const fn from_ns(ns: u64) -> Self {
        Self(ns)
    }

    /// Относительный дедлайн в микросекундах. Переполнение наносекунд насыщает значение
    /// до [`Self::INFINITE`].
    pub const fn from_micros(micros: u64) -> Self {
        Self(micros.saturating_mul(NS_PER_MICRO))
    }

    /// Относительный дедлайн в миллисекундах. Переполнение наносекунд насыщает значение
    /// до [`Self::INFINITE`].
    pub const fn from_millis(millis: u64) -> Self {
        Self(millis.saturating_mul(NS_PER_MILLI))
    }

    /// Относительный дедлайн в секундах. Переполнение наносекунд насыщает значение до
    /// [`Self::INFINITE`].
    pub const fn from_secs(secs: u64) -> Self {
        Self(secs.saturating_mul(NS_PER_SEC))
    }

    /// Возвращает сумму сроков. Сумма с [`Self::INFINITE`] либо сверх диапазона наносекунд -
    /// [`Self::INFINITE`].
    #[must_use]
    pub const fn saturating_add(self, other: Self) -> Self {
        Self(self.0.saturating_add(other.0))
    }

    /// Возвращает остаток срока после `other`. Остаток [`Self::INFINITE`] - [`Self::INFINITE`],
    /// остаток срока не длиннее `other` - [`Self::POLL`].
    #[must_use]
    pub const fn saturating_sub(self, other: Self) -> Self {
        if self.0 == Self::INFINITE.0 {
            return self;
        }
        Self(self.0.saturating_sub(other.0))
    }

    /// ABI-значение `timeout_ns` для укладки в аргумент syscall.
    pub const fn raw(self) -> u64 {
        self.0
    }
}

/// ABI-значение `timeout_ns` блокирующих Port-syscall "ждать бессрочно".
/// Sentinel `u64::MAX` отображается ядром в бессрочную блокировку и не занимает
/// слот в sleeper-heap.
pub const PORT_TIMEOUT_INFINITE: u64 = Timeout::INFINITE.raw();

/// ABI-значение `timeout_ns` блокирующих Port-syscall "не блокироваться"
/// (poll). Операция завершается немедленно, и без готовой встречной стороны возврат -
/// [`crate::SYSCALL_RETURN_TIMEOUT`].
pub const PORT_TIMEOUT_POLL: u64 = Timeout::POLL.raw();

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn units_scale_to_nanoseconds() {
        assert_eq!(Timeout::from_ns(5_000_000).raw(), 5_000_000);
        assert_eq!(Timeout::from_micros(50).raw(), 50_000);
        assert_eq!(Timeout::from_millis(5).raw(), 5_000_000);
        assert_eq!(Timeout::from_millis(500).raw(), 500_000_000);
        assert_eq!(Timeout::from_secs(3).raw(), 3_000_000_000);
    }

    #[test]
    fn overflowing_unit_saturates() {
        assert_eq!(Timeout::from_micros(u64::MAX), Timeout::INFINITE);
        assert_eq!(Timeout::from_millis(u64::MAX), Timeout::INFINITE);
        assert_eq!(Timeout::from_secs(u64::MAX), Timeout::INFINITE);
    }

    #[test]
    fn remainder_of_infinite_stays_infinite() {
        assert_eq!(
            Timeout::INFINITE.saturating_sub(Timeout::from_secs(1)),
            Timeout::INFINITE
        );
    }
}
