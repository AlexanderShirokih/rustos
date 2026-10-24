//! Монотонные часы процесса.
//!
//! Показания идут по тому же источнику и от той же точки отсчёта, что и дедлайны `timeout_ns`
//! блокирующих операций, и разность двух замеров сравнима с переданным тайм-аутом.
//!
//! Счётчик читается только под RustOS на aarch64. На другом таргете часы стоят на нуле, и
//! крейт собирается для хостовых тестов зависимых крейтов.

#[cfg(all(target_os = "molecular", target_arch = "aarch64"))]
use core::arch::asm;
use core::{
    sync::atomic::{AtomicU64, Ordering::Relaxed},
    time::Duration,
};

use syscall::Timeout;

const NS_PER_SEC: u128 = 1_000_000_000;

static CACHED_FREQUENCY: AtomicU64 = AtomicU64::new(0);

/// Момент монотонных часов.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Instant(u64);

impl Instant {
    /// Снимает текущее показание часов.
    #[must_use]
    pub fn now() -> Self {
        Self(ticks_to_ns(counter(), frequency()))
    }

    /// Момент по наносекундам от точки отсчёта часов.
    #[must_use]
    pub const fn from_nanos(ns: u64) -> Self {
        Self(ns)
    }

    /// Наносекунды от точки отсчёта часов.
    #[must_use]
    pub const fn as_nanos(self) -> u64 {
        self.0
    }

    /// Время, прошедшее с `earlier`. Момент позже текущего даёт [`Timeout::POLL`].
    #[must_use]
    pub const fn since(self, earlier: Self) -> Timeout {
        Timeout::from_ns(self.0.saturating_sub(earlier.0))
    }

    /// Момент через `duration`. `None` - момент за пределом часов.
    #[must_use]
    pub fn checked_add(self, duration: Duration) -> Option<Self> {
        let ns = u64::try_from(duration.as_nanos()).ok()?;
        self.0.checked_add(ns).map(Self)
    }

    /// Момент через срок `after`. `None` - срок бессрочный либо за пределом часов.
    #[must_use]
    pub const fn after(self, after: Timeout) -> Option<Self> {
        if after.raw() == Timeout::INFINITE.raw() {
            return None;
        }
        match self.0.checked_add(after.raw()) {
            Some(ns) => Some(Self(ns)),
            None => None,
        }
    }

    /// Период счётчика часов, округлённый вверх до наносекунды.
    #[must_use]
    pub fn resolution() -> Duration {
        let period = NS_PER_SEC.div_ceil(u128::from(frequency().max(1)));
        Duration::from_nanos(u64::try_from(period).unwrap_or(u64::MAX))
    }
}

const fn ticks_to_ns(ticks: u64, frequency: u64) -> u64 {
    if frequency == 0 {
        return 0;
    }
    #[allow(clippy::cast_possible_truncation)]
    let ns = (ticks as u128 * NS_PER_SEC / frequency as u128) as u64;
    ns
}

fn frequency() -> u64 {
    // Частота задана платформой на всё время работы, поэтому гонка на заполнении кэша
    // безобидна. Обе стороны пишут одно значение, и кэш каждой сборки крейта в процессе
    // хранит то же значение.
    let cached = CACHED_FREQUENCY.load(Relaxed);
    if cached != 0 {
        return cached;
    }
    let value = read_frequency();
    CACHED_FREQUENCY.store(value, Relaxed);
    value
}

#[cfg(all(target_os = "molecular", target_arch = "aarch64"))]
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

#[cfg(all(target_os = "molecular", target_arch = "aarch64"))]
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

#[cfg(not(all(target_os = "molecular", target_arch = "aarch64")))]
fn counter() -> u64 {
    0
}

#[cfg(not(all(target_os = "molecular", target_arch = "aarch64")))]
fn read_frequency() -> u64 {
    1
}
