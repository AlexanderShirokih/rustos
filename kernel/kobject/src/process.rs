//! `Process` kernel object: lifecycle-объект процесса.

use alloc::sync::Arc;

use super::{signal::Signal, termination::TerminationState};

pub struct ProcessObject {
    inner: TerminationState,
}

impl ProcessObject {
    /// Создаёт новый `ProcessObject`.
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            inner: TerminationState::new(),
        })
    }

    /// Идемпотентно публикует `code` и помечает процесс завершённым.
    /// Повторный вызов - no-op: первый победитель фиксирует exit_code.
    pub fn signal_terminated(&self, code: i32) {
        self.inner.signal_terminated(code);
    }

    /// Код завершения процесса. До завершения возвращает 0.
    pub fn exit_code(&self) -> i32 {
        self.inner.exit_code()
    }

    /// Завершён ли процесс.
    pub fn terminated(&self) -> bool {
        self.inner.terminated()
    }

    /// Ленивый bound-[`Signal`] термнинации (бит `SIGNALED`). Материализуется
    /// при первом вызове и пре-сигналится, если процесс уже завершён.
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
        let proc = ProcessObject::new();
        let sig = proc.termination_signal();
        let w = MockWaker::new();
        sig.register_waiter(SIGNALED, w.clone());

        assert!(!w.was_woken());
        proc.signal_terminated(42);
        assert!(w.was_woken());
        assert_eq!(w.observed() & SIGNALED, SIGNALED);
        assert_eq!(proc.exit_code(), 42);
        assert!(proc.terminated());
    }

    #[test]
    fn signal_terminated_is_idempotent() {
        let proc = ProcessObject::new();
        proc.signal_terminated(7);
        proc.signal_terminated(99);
        assert_eq!(proc.exit_code(), 7);
        assert!(proc.terminated());
    }

    #[test]
    fn exit_code_zero_before_termination() {
        let proc = ProcessObject::new();
        assert_eq!(proc.exit_code(), 0);
        assert!(!proc.terminated());
    }

    #[test]
    fn late_termination_signal_is_presignaled() {
        let proc = ProcessObject::new();
        proc.signal_terminated(5);
        // Наблюдатель подписался уже после выхода: Signal сразу несёт SIGNALED.
        let sig = proc.termination_signal();
        assert_eq!(sig.peek() & SIGNALED, SIGNALED);
    }

    #[test]
    fn koid_unique_per_instance() {
        use crate::object::KObject;
        let a = ProcessObject::new();
        let b = ProcessObject::new();
        assert_ne!(KObject::Process(a).koid(), KObject::Process(b).koid());
    }
}
