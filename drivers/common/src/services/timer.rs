//! Контракты подсистемы системного таймера.

use crate::services::Service;

/// Контракт сервиса системного таймера.
pub trait TimerService: Service {
    /// Возвращает текущее значение аппаратного счётчика.
    fn time_monotonic_elapsed(&self) -> u64;

    fn set_periodic(&self, interval_ms: u64);
}
