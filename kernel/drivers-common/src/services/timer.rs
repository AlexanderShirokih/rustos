//! Контракты подсистемы системного таймера.

use alloc::sync::Arc;

const NS_PER_SEC: u128 = 1_000_000_000;

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
    ///
    /// Прошедший дедлайн срабатывает сразу. Дедлайн `u64::MAX` за время работы
    /// системы не наступает и оставляет таймер без срабатывания.
    fn schedule_next(&self, deadline_ns: u64);

    /// Регистрирует системный обработчик тиков.
    fn set_handler(&self, handler: Arc<dyn TickHandler>);
}

/// Наименьшее показание счётчика частоты `frequency` Гц, при котором момент
/// `deadline_ns` уже наступил.
///
/// Показание, переведённое обратно через [`ticks_to_ns`], не меньше `deadline_ns`.
/// Показание за пределами счётчика насыщается до `u64::MAX`.
pub fn deadline_ticks(deadline_ns: u64, frequency: u64) -> u64 {
    let ticks = (u128::from(deadline_ns) * u128::from(frequency)).div_ceil(NS_PER_SEC);

    u64::try_from(ticks).unwrap_or(u64::MAX)
}

/// Момент в наносекундах, которому соответствует показание `ticks` счётчика
/// частоты `frequency` Гц. Округление идёт вниз, нулевая частота даёт ноль.
/// Момент за пределами `u64` насыщается до `u64::MAX`.
pub fn ticks_to_ns(ticks: u64, frequency: u64) -> u64 {
    let Some(ns) = (u128::from(ticks) * NS_PER_SEC).checked_div(u128::from(frequency)) else {
        return 0;
    };

    u64::try_from(ns).unwrap_or(u64::MAX)
}
