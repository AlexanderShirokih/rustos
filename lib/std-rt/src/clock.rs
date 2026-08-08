//! Монотонные часы процесса.
//!
//! Показания идут по тому же источнику и от той же точки отсчёта, что и дедлайны `timeout_ns`
//! блокирующих операций, и разность двух замеров сравнима с переданным тайм-аутом.

use core::{
    arch::asm,
    sync::atomic::{AtomicU64, Ordering},
};

use syscall::Timeout;

const NS_PER_SEC: u128 = 1_000_000_000;

static CACHED_FREQUENCY: AtomicU64 = AtomicU64::new(0);

/// Момент монотонных часов.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Instant(u64);

impl Instant {
    /// Снимает текущее показание часов.
    pub fn now() -> Self {
        Self(ticks_to_ns(counter(), frequency()))
    }

    /// Наносекунды от точки отсчёта часов.
    pub const fn as_nanos(self) -> u64 {
        self.0
    }

    /// Время, прошедшее с `earlier`. Момент позже текущего даёт [`Timeout::POLL`].
    pub const fn since(self, earlier: Self) -> Timeout {
        Timeout::from_ns(self.0.saturating_sub(earlier.0))
    }
}

const fn ticks_to_ns(ticks: u64, frequency: u64) -> u64 {
    if frequency == 0 {
        return 0;
    }
    (ticks as u128 * NS_PER_SEC / frequency as u128) as u64
}

fn frequency() -> u64 {
    // Частота задана платформой на всё время работы, поэтому гонка на заполнении кэша
    // безобидна: обе стороны пишут одно значение.
    let cached = CACHED_FREQUENCY.load(Ordering::Relaxed);
    if cached != 0 {
        return cached;
    }
    let value = read_frequency();
    CACHED_FREQUENCY.store(value, Ordering::Relaxed);
    value
}

fn counter() -> u64 {
    let ticks: u64;
    // SAFETY: чтение счётчика ядро разрешает процессу на всё время его работы; инструкция
    // не трогает память.
    unsafe {
        asm!(
            "mrs {ticks}, cntvct_el0",
            ticks = out(reg) ticks,
            options(nomem, nostack, preserves_flags),
        );
    }
    ticks
}

fn read_frequency() -> u64 {
    let frequency: u64;
    // SAFETY: частота читается по тому же разрешению, что и счётчик; инструкция не трогает
    // память.
    unsafe {
        asm!(
            "mrs {frequency}, cntfrq_el0",
            frequency = out(reg) frequency,
            options(nomem, nostack, preserves_flags),
        );
    }
    frequency
}
