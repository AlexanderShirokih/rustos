//! `Timer` KO: однобитовый сигнальный объект, переключаемый
//! "owner"-стороной (timer-server) при наступлении дедлайна.
//!
//! Реализация поверх [`SignalState`] - никакой собственной интеграции
//! со SleepQueue здесь нет: timer-server-поток сам ждёт через
//! `SchedulerService::sleep_ns` и зовёт [`Timer::fire`] / [`Timer::arm`].
//!
//! API:
//! - [`Timer::arm`] - снимает `SIGNALED` (готовит к следующему дедлайну).
//! - [`Timer::fire`] - поднимает `SIGNALED` и будит ожидающих.
//! - [`Timer::cancel`] - алиас `arm` для семантической ясности.

use alloc::sync::Arc;

use super::{kernel_object::KernelObject, object_type::ObjectType, wait::SignalState};

/// Главный сигнал "timer expired".
pub const TIMER_SIGNALED: u32 = 1 << 0;

pub struct Timer {
    signals: SignalState,
}

impl Timer {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            signals: SignalState::new(0),
        })
    }

    /// Снимает `SIGNALED`. Вызывается перед перезапуском периодического
    /// дедлайна, чтобы следующий `object_wait_one` действительно ждал
    /// нового тика, а не возвращался немедленно по "прошлому" биту.
    pub fn arm(&self) {
        self.signals.signal(0, TIMER_SIGNALED);
    }

    /// Семантический алиас [`Self::arm`].
    pub fn cancel(&self) {
        self.arm();
    }

    /// Поднимает `SIGNALED` и будит ожидающих.
    pub fn fire(&self) {
        self.signals.signal(TIMER_SIGNALED, 0);
    }

    pub fn peek(&self) -> u32 {
        self.signals.peek()
    }

    pub fn signal_state(&self) -> &SignalState {
        &self.signals
    }
}

impl KernelObject for Timer {
    fn object_type(&self) -> ObjectType {
        ObjectType::Timer
    }

    fn signal_state(&self) -> Option<&SignalState> {
        Some(&self.signals)
    }

    fn as_timer(&self) -> Option<&Timer> {
        Some(self)
    }
}

#[cfg(test)]
mod tests {
    use super::{super::wait::MockWaker, *};

    #[test]
    fn fire_sets_signaled_and_wakes() {
        let t = Timer::new();
        let w = MockWaker::new();
        t.signal_state().register_waiter(TIMER_SIGNALED, w.clone());
        assert!(!w.was_woken());

        t.fire();
        assert!(w.was_woken());
        assert_eq!(t.peek() & TIMER_SIGNALED, TIMER_SIGNALED);
    }

    #[test]
    fn arm_clears_signaled() {
        let t = Timer::new();
        t.fire();
        assert_eq!(t.peek() & TIMER_SIGNALED, TIMER_SIGNALED);
        t.arm();
        assert_eq!(t.peek() & TIMER_SIGNALED, 0);
    }

    #[test]
    fn metadata_matches() {
        let t = Timer::new();
        assert_eq!(t.object_type(), ObjectType::Timer);
        let other = Timer::new();
        assert_ne!(t.koid(), other.koid());
    }
}
