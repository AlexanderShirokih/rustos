/// Типизированный фасад над `timeout_ns` блокирующих syscall'ов.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(transparent)]
pub struct Timeout(u64);

impl Timeout {
    /// Не блокироваться: операция завершается немедленно, иначе возвращается
    /// [`SYSCALL_RETURN_TIMEOUT`].
    pub const POLL: Self = Self(0);

    /// Ждать бессрочно (не занимает слот в sleeper-heap).
    pub const INFINITE: Self = Self(u64::MAX);

    /// Дедлайн в наносекундах.
    pub const fn from_ns(ns: u64) -> Self {
        Self(ns)
    }

    /// ABI-значение `timeout_ns` для укладки в аргумент syscall'а.
    pub const fn raw(self) -> u64 {
        self.0
    }
}

/// ABI-значение `timeout_ns` блокирующих Port-syscall'ов "ждать бессрочно".
/// Sentinel `u64::MAX` отображается ядром в бессрочную блокировку и не занимает
/// слот в sleeper-heap.
pub const PORT_TIMEOUT_INFINITE: u64 = Timeout::INFINITE.raw();

/// ABI-значение `timeout_ns` блокирующих Port-syscall'ов "не блокироваться"
/// (poll): операция завершается немедленно, иначе возвращается
/// [`SYSCALL_RETURN_TIMEOUT`].
pub const PORT_TIMEOUT_POLL: u64 = Timeout::POLL.raw();
