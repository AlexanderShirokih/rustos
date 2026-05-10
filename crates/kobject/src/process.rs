//! `Process` KO: lifecycle-объект процесса с битом `PROCESS_TERMINATED`.
//!
//! Сигнал поднимает только ядро в момент завершения процесса; пользователь
//! `Rights::SIGNAL` на handle'е не получает (см. [`Rights::defaults_for`]).
//! Exit-код публикуется до подъёма сигнала, чтобы наблюдатель, увидевший
//! `PROCESS_TERMINATED`, гарантированно прочитал финальное значение.

use alloc::sync::Arc;
use core::sync::atomic::{AtomicI32, Ordering};

use super::wait::SignalState;

/// Сигнал "процесс завершён". Поднимается ровно один раз.
pub const PROCESS_TERMINATED: u32 = 1 << 0;

pub struct ProcessObject {
    signals: SignalState,
    exit_code: AtomicI32,
}

impl ProcessObject {
    /// Создаёт новый `ProcessObject` без поднятых сигналов и с нулевым кодом.
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            signals: SignalState::new(0),
            exit_code: AtomicI32::new(0),
        })
    }

    /// Идемпотентно публикует `code` и поднимает [`PROCESS_TERMINATED`].
    /// Повторный вызов - no-op: первый победитель фиксирует exit_code.
    pub fn signal_terminated(&self, code: i32) {
        // Release на exit_code публикует значение до подъёма сигнала;
        // парный Acquire в `exit_code()` гарантирует видимость кода
        // тому, кто увидел сигнал.
        if self.signals.peek() & PROCESS_TERMINATED != 0 {
            return;
        }
        self.exit_code.store(code, Ordering::Release);
        self.signals.signal(PROCESS_TERMINATED, 0);
    }

    /// Финальный exit-код. До подъёма [`PROCESS_TERMINATED`] возвращает 0.
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
