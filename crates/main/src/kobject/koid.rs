use core::{
    num::NonZeroU64,
    sync::atomic::{AtomicU64, Ordering},
};

/// Глобально-уникальный идентификатор kernel-объекта. Служит только для логов и сравнения.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Koid(NonZeroU64);

impl Koid {
    /// Выделяет новый идентификатор.
    pub fn allocate() -> Self {
        static COUNTER: AtomicU64 = AtomicU64::new(1);
        let raw = COUNTER.fetch_add(1, Ordering::Relaxed);
        Self(NonZeroU64::new(raw).expect("Koid counter exhausted"))
    }

    pub fn raw(self) -> u64 {
        self.0.get()
    }
}
