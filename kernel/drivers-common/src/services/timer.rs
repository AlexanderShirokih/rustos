//! Контракты подсистемы системного таймера.

use alloc::sync::Arc;

/// Получатель timer tick событий.
pub trait TickHandler: Send + Sync {
    /// Вызывается из обработчика таймера с уже прочитанным монотонным временем.
    fn on_tick(&self, now_ns: u64);
}

/// Контракт сервиса системного таймера.
pub trait TimerService: Send + Sync {
    /// Возвращает текущее монотонное время в наносекундах.
    fn now_ns(&self) -> u64;

    /// Программирует следующий дедлайн таймера в абсолютном времени.
    fn schedule_next(&self, deadline_ns: u64);

    /// Регистрирует системный обработчик тиков.
    fn set_handler(&self, handler: Arc<dyn TickHandler>);
}
