//! Приоритет потока в аргументе `ThreadCreate`.

/// Приоритет потока занимает биты [0..8) аргумента ABI, старшие биты нулевые. Ядро
/// отвергает взведённый бит [8..) как `InvalidArgument`, и значение из [`Priority`] такой
/// аргумент собрать не позволяет.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(transparent)]
pub struct Priority(u8);

impl Priority {
    #[must_use]
    pub const fn new(value: u8) -> Self {
        Self(value)
    }
}

impl From<Priority> for u64 {
    fn from(priority: Priority) -> Self {
        Self::from(priority.0)
    }
}
