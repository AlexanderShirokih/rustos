//! `Thread` KO: lifecycle-объект потока с битом `THREAD_TERMINATED`.
//!
//! Симметричен [`ProcessObject`](super::process::ProcessObject): сигнал
//! поднимает только ядро, пользователь `Rights::SIGNAL` не получает.
//! Exit-код публикуется до подъёма сигнала, чтобы наблюдатель, увидевший
//! `THREAD_TERMINATED`, гарантированно прочитал финальное значение.

use alloc::sync::Arc;

pub use syscall::THREAD_TERMINATED;

use super::{
    termination::TerminationState,
    wait::{SignalSource, SignalState},
};

pub struct ThreadObject {
    inner: TerminationState,
}

impl ThreadObject {
    /// Создаёт новый `ThreadObject` без поднятых сигналов и с нулевым кодом.
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            inner: TerminationState::new(),
        })
    }

    /// Идемпотентно публикует `code` и поднимает [`THREAD_TERMINATED`].
    /// Повторный вызов - no-op: первый победитель фиксирует exit_code.
    pub fn signal_terminated(&self, code: i32) {
        self.inner.signal_terminated(THREAD_TERMINATED, code);
    }

    /// Финальный exit-код. До подъёма [`THREAD_TERMINATED`] возвращает 0.
    pub fn exit_code(&self) -> i32 {
        self.inner.exit_code()
    }

    /// Прямой доступ к [`SignalState`] для интеграции с `object_wait_one`.
    pub fn signals(&self) -> &SignalState {
        self.inner.signals()
    }

    /// Текущий снимок сигналов (без блокировок).
    pub fn peek(&self) -> u32 {
        self.inner.peek()
    }
}

impl SignalSource for ThreadObject {
    fn signals(&self) -> &SignalState {
        self.inner.signals()
    }
}

#[cfg(test)]
mod tests {
    use super::{super::wait::MockWaker, *};

    #[test]
    fn signal_terminated_wakes_observer() {
        let th = ThreadObject::new();
        let w = MockWaker::new();
        th.signals().register_waiter(THREAD_TERMINATED, w.clone());

        assert!(!w.was_woken());
        th.signal_terminated(-1);
        assert!(w.was_woken());
        assert_eq!(w.observed() & THREAD_TERMINATED, THREAD_TERMINATED);
        assert_eq!(th.exit_code(), -1);
    }

    #[test]
    fn signal_terminated_is_idempotent() {
        let th = ThreadObject::new();
        th.signal_terminated(7);
        th.signal_terminated(99);
        assert_eq!(th.exit_code(), 7);
        assert_eq!(th.peek() & THREAD_TERMINATED, THREAD_TERMINATED);
    }

    #[test]
    fn exit_code_zero_before_termination() {
        let th = ThreadObject::new();
        assert_eq!(th.exit_code(), 0);
        assert_eq!(th.peek(), 0);
    }

    #[test]
    fn koid_unique_per_instance() {
        use crate::object::KObject;
        let a = ThreadObject::new();
        let b = ThreadObject::new();
        assert_ne!(KObject::Thread(a).koid(), KObject::Thread(b).koid());
    }
}
