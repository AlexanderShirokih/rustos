//! Общий lifecycle-стейт Process/Thread: exit-код + bound-`Signal`.
//!
//! Завершение наблюдается через [`Signal`], материализуемый по требованию (`termination_signal`): ненаблюдаемый объект не аллоцирует `Signal` вовсе и

use alloc::sync::Arc;
use core::sync::atomic::{AtomicBool, AtomicI32, Ordering};

use collections::{LockCell, MutexCell};

use super::signal::{SIGNALED, Signal};

pub(crate) struct TerminationState {
    terminated: AtomicBool,
    exit_code: AtomicI32,
    signal: MutexCell<Option<Arc<Signal>>>,
}

impl TerminationState {
    pub(crate) fn new() -> Self {
        Self {
            terminated: AtomicBool::new(false),
            exit_code: AtomicI32::new(0),
            signal: MutexCell::new(None),
        }
    }

    /// Идемпотентно публикует `code`, помечает объект завершённым и, если
    /// bound-`Signal` уже материализован, поднимает на нём `SIGNALED`.
    pub(crate) fn signal_terminated(&self, code: i32) {
        let to_signal = self.signal.with_lock(|slot| {
            if self.terminated.load(Ordering::Acquire) {
                return None;
            }
            self.exit_code.store(code, Ordering::Release);
            self.terminated.store(true, Ordering::Release);
            slot.clone()
        });

        if let Some(signal) = to_signal {
            signal.signal(SIGNALED, 0);
        }
    }

    /// Завершён ли объект.
    pub(crate) fn terminated(&self) -> bool {
        self.terminated.load(Ordering::Acquire)
    }

    /// Финальный exit-код. До завершения возвращает 0.
    pub(crate) fn exit_code(&self) -> i32 {
        self.exit_code.load(Ordering::Acquire)
    }

    /// Возвращает `Signal` терминации, создавая его при первом вызове. Если объект уже завершён, 
    /// свежесозданный `Signal` сразу несёт `SIGNALED`.
    pub(crate) fn termination_signal(&self) -> Arc<Signal> {
        self.signal.with_lock(|slot| {
            if let Some(signal) = slot.as_ref() {
                return signal.clone();
            }
            let initial = if self.terminated.load(Ordering::Acquire) {
                SIGNALED
            } else {
                0
            };
            let signal = Arc::new(Signal::with_bits(initial));
            *slot = Some(signal.clone());
            signal
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{super::wait::MockWaker, *};

    #[test]
    fn fresh_state_is_not_terminated_with_zero_code() {
        let s = TerminationState::new();
        assert!(!s.terminated());
        assert_eq!(s.exit_code(), 0);
    }

    #[test]
    fn signal_terminated_publishes_code_and_flag() {
        let s = TerminationState::new();
        s.signal_terminated(42);
        assert!(s.terminated());
        assert_eq!(s.exit_code(), 42);
    }

    #[test]
    fn signal_terminated_is_idempotent_first_writer_wins() {
        let s = TerminationState::new();
        s.signal_terminated(7);
        s.signal_terminated(99);
        assert_eq!(s.exit_code(), 7);
        assert!(s.terminated());
    }

    #[test]
    fn materialized_signal_wakes_observer_on_terminate() {
        let s = TerminationState::new();
        let sig = s.termination_signal();
        let w = MockWaker::new();
        sig.register_waiter(SIGNALED, w.clone());

        assert!(!w.was_woken());
        s.signal_terminated(5);
        assert!(w.was_woken());
        assert_eq!(w.observed() & SIGNALED, SIGNALED);
    }

    #[test]
    fn late_observer_sees_presignaled_signal() {
        let s = TerminationState::new();
        s.signal_terminated(3);
        let sig = s.termination_signal();
        assert_eq!(sig.peek() & SIGNALED, SIGNALED);
    }

    #[test]
    fn termination_signal_is_cached_across_calls() {
        let s = TerminationState::new();
        let a = s.termination_signal();
        let b = s.termination_signal();
        assert!(Arc::ptr_eq(&a, &b));
    }
}
