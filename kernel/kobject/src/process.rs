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
    use super::{super::signal::SIGNALED, *};

    #[test]
    fn process_delegates_lifecycle_to_termination_state() {
        let proc = ProcessObject::new();
        assert!(!proc.terminated());
        assert_eq!(proc.exit_code(), 0);

        proc.signal_terminated(42);
        assert!(proc.terminated());
        assert_eq!(proc.exit_code(), 42);
        assert_eq!(proc.termination_signal().peek() & SIGNALED, SIGNALED);
    }
}
