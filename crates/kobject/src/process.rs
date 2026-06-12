//! `Process` KO: lifecycle-объект процесса с битом `PROCESS_TERMINATED`.
//!
//! Сигнал поднимает только ядро в момент завершения процесса; пользователь
//! `Rights::SIGNAL` на handle'е не получает (см. [`Rights::defaults_for`]).
//! Exit-код публикуется до подъёма сигнала, чтобы наблюдатель, увидевший
//! `PROCESS_TERMINATED`, гарантированно прочитал финальное значение.

use alloc::sync::Arc;

use super::{
    termination::TerminationState,
    wait::{SignalSource, SignalState},
};

/// Сигнал "процесс завершён". Поднимается ровно один раз.
pub const PROCESS_TERMINATED: u32 = 1 << 0;

pub struct ProcessObject {
    inner: TerminationState,
}

impl ProcessObject {
    /// Создаёт новый `ProcessObject` без поднятых сигналов и с нулевым кодом.
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            inner: TerminationState::new(),
        })
    }

    /// Идемпотентно публикует `code` и поднимает [`PROCESS_TERMINATED`].
    /// Повторный вызов - no-op: первый победитель фиксирует exit_code.
    pub fn signal_terminated(&self, code: i32) {
        self.inner.signal_terminated(PROCESS_TERMINATED, code);
    }

    /// Финальный exit-код. До подъёма [`PROCESS_TERMINATED`] возвращает 0.
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

impl SignalSource for ProcessObject {
    fn signals(&self) -> &SignalState {
        self.inner.signals()
    }
}

#[cfg(test)]
mod tests {
    use super::{super::wait::MockWaker, *};

    #[test]
    fn signal_terminated_wakes_observer() {
        let proc = ProcessObject::new();
        let w = MockWaker::new();
        proc.signals()
            .register_waiter(PROCESS_TERMINATED, w.clone());

        assert!(!w.was_woken());
        proc.signal_terminated(42);
        assert!(w.was_woken());
        assert_eq!(w.observed() & PROCESS_TERMINATED, PROCESS_TERMINATED);
        assert_eq!(proc.exit_code(), 42);
    }

    #[test]
    fn signal_terminated_is_idempotent() {
        let proc = ProcessObject::new();
        proc.signal_terminated(7);
        proc.signal_terminated(99);
        assert_eq!(proc.exit_code(), 7);
        assert_eq!(proc.peek() & PROCESS_TERMINATED, PROCESS_TERMINATED);
    }

    #[test]
    fn exit_code_zero_before_termination() {
        let proc = ProcessObject::new();
        assert_eq!(proc.exit_code(), 0);
        assert_eq!(proc.peek(), 0);
    }

    #[test]
    fn koid_unique_per_instance() {
        use crate::object::KObject;
        let a = ProcessObject::new();
        let b = ProcessObject::new();
        assert_ne!(KObject::Process(a).koid(), KObject::Process(b).koid());
    }
}
