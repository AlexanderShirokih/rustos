//! `Thread` KO: lifecycle-объект потока с битом `THREAD_TERMINATED`.
//!
//! Симметричен [`ProcessObject`](super::process::ProcessObject): сигнал
//! поднимает только ядро, пользователь `Rights::SIGNAL` не получает.
//! Exit-код публикуется до подъёма сигнала, чтобы наблюдатель, увидевший
//! `THREAD_TERMINATED`, гарантированно прочитал финальное значение.

use alloc::sync::Arc;
use core::sync::atomic::{AtomicI32, Ordering};

use super::wait::SignalState;

/// Сигнал "поток завершён". Поднимается ровно один раз.
pub const THREAD_TERMINATED: u32 = 1 << 0;

pub struct ThreadObject {
    signals: SignalState,
    exit_code: AtomicI32,
}

impl ThreadObject {
    /// Создаёт новый `ThreadObject` без поднятых сигналов и с нулевым кодом.
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            signals: SignalState::new(0),
            exit_code: AtomicI32::new(0),
        })
    }

    /// Идемпотентно публикует `code` и поднимает [`THREAD_TERMINATED`].
    /// Повторный вызов - no-op: первый победитель фиксирует exit_code.
    pub fn signal_terminated(&self, code: i32) {
        // Release на exit_code публикует значение до подъёма сигнала;
        // парный Acquire в `exit_code()` гарантирует видимость кода
        // тому, кто увидел сигнал.
        if self.signals.peek() & THREAD_TERMINATED != 0 {
            return;
        }
        self.exit_code.store(code, Ordering::Release);
        self.signals.signal(THREAD_TERMINATED, 0);
    }

    /// Финальный exit-код. До подъёма [`THREAD_TERMINATED`] возвращает 0.
    pub fn exit_code(&self) -> i32 {
        self.exit_code.load(Ordering::Acquire)
    }

    /// Прямой доступ к [`SignalState`] для интеграции с `object_wait_one`.
    pub fn signals(&self) -> &SignalState {
        &self.signals
    }

    /// Текущий снимок сигналов (без блокировок).
    pub fn peek(&self) -> u32 {
        self.signals.peek()
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
