//! `Thread` kernel object: lifecycle-объект потока.
//!
//! Симметричен [`ProcessObject`](super::process::ProcessObject): завершение
//! наблюдается через bound-[`Signal`] (`termination_signal`).

use alloc::sync::Arc;

use super::{signal::Signal, termination::TerminationState};

pub struct ThreadObject {
    inner: TerminationState,
}

impl ThreadObject {
    /// Создаёт новый `ThreadObject`.
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            inner: TerminationState::new(),
        })
    }

    /// Идемпотентно публикует `code` и помечает поток завершённым.
    pub fn signal_terminated(&self, code: i32) {
        self.inner.signal_terminated(code);
    }

    /// Финальный exit-код. До завершения возвращает 0.
    pub fn exit_code(&self) -> i32 {
        self.inner.exit_code()
    }

    /// Завершён ли поток.
    pub fn terminated(&self) -> bool {
        self.inner.terminated()
    }

    /// Ленивый bound-[`Signal`] термнинации (бит `SIGNALED`). Материализуется
    /// при первом вызове и пре-сигналится, если поток уже завершён.
    pub fn termination_signal(&self) -> Arc<Signal> {
        self.inner.termination_signal()
    }
}

#[cfg(test)]
mod tests {
    use super::{
        super::{signal::SIGNALED, wait::MockWaker},
        *,
    };

    #[test]
    fn signal_terminated_wakes_observer() {
        let th = ThreadObject::new();
        let sig = th.termination_signal();
        let w = MockWaker::new();
        sig.register_waiter(SIGNALED, w.clone());

        assert!(!w.was_woken());
        th.signal_terminated(-1);
        assert!(w.was_woken());
        assert_eq!(w.observed() & SIGNALED, SIGNALED);
        assert_eq!(th.exit_code(), -1);
        assert!(th.terminated());
    }

    #[test]
    fn signal_terminated_is_idempotent() {
        let th = ThreadObject::new();
        th.signal_terminated(7);
        th.signal_terminated(99);
        assert_eq!(th.exit_code(), 7);
        assert!(th.terminated());
    }

    #[test]
    fn exit_code_zero_before_termination() {
        let th = ThreadObject::new();
        assert_eq!(th.exit_code(), 0);
        assert!(!th.terminated());
    }

    #[test]
    fn late_termination_signal_is_presignaled() {
        let th = ThreadObject::new();
        th.signal_terminated(3);
        let sig = th.termination_signal();
        assert_eq!(sig.peek() & SIGNALED, SIGNALED);
    }

    #[test]
    fn koid_unique_per_instance() {
        use crate::object::KObject;
        let a = ThreadObject::new();
        let b = ThreadObject::new();
        assert_ne!(KObject::Thread(a).koid(), KObject::Thread(b).koid());
    }
}
