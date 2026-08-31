//! Приоритет потока в аргументе `ThreadCreate`.

/// Приоритет потока: занимает биты [0..8) ABI, биты [8..32) нулевые по
/// построению, и отдельная проверка `priority > 0xFF` не нужна.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(transparent)]
pub struct Priority(u8);

impl Priority {
    /// Оборачивает значение приоритета.
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
