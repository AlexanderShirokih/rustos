//! Снимок часов реального времени поверх монотонных часов процесса [`Instant`].

use core::{
    sync::atomic::{
        AtomicU32, AtomicU64,
        Ordering::{Acquire, Relaxed, Release},
    },
    time::Duration,
};

pub use syscall_user::clock::Instant;

const REALTIME_EMPTY: u32 = 0;
const REALTIME_WRITING: u32 = 1;
const REALTIME_READY: u32 = 2;

static REALTIME_STATE: AtomicU32 = AtomicU32::new(REALTIME_EMPTY);
static REALTIME_SECONDS: AtomicU64 = AtomicU64::new(0);
static REALTIME_TAKEN: AtomicU64 = AtomicU64::new(0);

/// Записывает снимок часов реального времени: `seconds` от эпохи Unix в текущий момент
/// монотонных часов. `false` - снимок уже записан.
pub fn install_realtime(seconds: u64) -> bool {
    let taken = Instant::now();
    if REALTIME_STATE
        .compare_exchange(REALTIME_EMPTY, REALTIME_WRITING, Acquire, Relaxed)
        .is_err()
    {
        return false;
    }

    REALTIME_SECONDS.store(seconds, Relaxed);
    REALTIME_TAKEN.store(taken.as_nanos(), Relaxed);
    REALTIME_STATE.store(REALTIME_READY, Release);
    true
}

/// Время от эпохи Unix, отсчитанное по монотонным часам от снимка [`install_realtime`].
/// `None` - снимок не записан.
pub fn realtime() -> Option<Duration> {
    if REALTIME_STATE.load(Acquire) != REALTIME_READY {
        return None;
    }

    let seconds = Duration::from_secs(REALTIME_SECONDS.load(Relaxed));
    let taken = Instant::from_nanos(REALTIME_TAKEN.load(Relaxed));
    let elapsed = Duration::from_nanos(Instant::now().since(taken).raw());
    Some(seconds.saturating_add(elapsed))
}
