//! `Thread` capability target: lifecycle-объект потока.
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

    /// Ленивый [`Signal`] терминации (бит `SIGNALED`). Материализуется
    /// при первом вызове и пре-сигналится, если поток уже завершён.
    pub fn termination_signal(&self) -> Arc<Signal> {
        self.inner.termination_signal()
    }
}

#[cfg(test)]
mod tests {
    use super::{super::signal::SIGNALED, *};

    #[test]
    fn thread_delegates_lifecycle_to_termination_state() {
        let th = ThreadObject::new();
        assert!(!th.terminated());
        assert_eq!(th.exit_code(), 0);

        th.signal_terminated(-1);
        assert!(th.terminated());
        assert_eq!(th.exit_code(), -1);
        assert_eq!(th.termination_signal().peek() & SIGNALED, SIGNALED);
    }
}
