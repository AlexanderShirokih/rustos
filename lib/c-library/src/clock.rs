//! Часы `CLOCK_REALTIME` и `CLOCK_MONOTONIC`, сон и сроки ожиданий библиотеки C поверх
//! монотонных часов и снимка часов реального времени процесса `runtime`.
//!
//! До установки часов реального времени [`install_realtime_clock`] чтение `CLOCK_REALTIME`
//! даёт `ENOTCONN`, а срок по нему - `EINVAL`.

use core::{
    ffi::{c_int, c_ulong},
    mem::MaybeUninit,
    time::Duration,
};

use component_startup::Session;
use rtc_abi::rtc;
use runtime::Instant;

use crate::errno::{Errno, fail};

pub(crate) type ClockId = c_ulong;

const NS_PER_SEC: u32 = 1_000_000_000;

// Значения определены в `time.c` константами и не меняются.
#[allow(unsafe_code)]
unsafe extern "C" {
    safe static c_library_clock_realtime: ClockId;
    safe static c_library_clock_monotonic: ClockId;
}

/// Отказ установки часов реального времени процесса.
#[derive(Debug)]
pub enum RealtimeClockError {
    /// Часы реального времени процесса уже установлены.
    AlreadyInstalled,
    /// Сессия `Rtc` не ответила временем.
    Unavailable,
}

#[derive(Clone, Copy)]
pub(crate) enum Clock {
    Realtime,
    Monotonic,
}

#[repr(C)]
struct Time {
    seconds: i64,
    nanoseconds: i64,
}

impl Clock {
    pub(crate) fn from_id(id: ClockId) -> Option<Self> {
        if id == c_library_clock_realtime {
            Some(Self::Realtime)
        } else if id == c_library_clock_monotonic {
            Some(Self::Monotonic)
        } else {
            None
        }
    }

    pub(crate) fn id(self) -> ClockId {
        match self {
            Self::Realtime => c_library_clock_realtime,
            Self::Monotonic => c_library_clock_monotonic,
        }
    }

    fn now(self) -> Option<Duration> {
        match self {
            Self::Realtime => runtime::realtime(),
            Self::Monotonic => Some(Duration::from_nanos(Instant::now().as_nanos())),
        }
    }
}

impl Time {
    fn of(duration: Duration) -> Self {
        Self {
            seconds: i64::try_from(duration.as_secs()).unwrap_or(i64::MAX),
            nanoseconds: i64::from(duration.subsec_nanos()),
        }
    }
}

/// Устанавливает часы реального времени процесса по времени сессии `Rtc`, которое должно
/// отсчитываться в секундах от эпохи Unix. Время читается один раз, дальше часы идут по
/// монотонным часам процесса. Сессия закрывается до возврата.
///
/// Отказы:
/// - [`RealtimeClockError::AlreadyInstalled`] - часы уже установлены, и время процесса
///   отсчитывается от первой установки;
/// - [`RealtimeClockError::Unavailable`] - вызов `now` сессии завершился отказом.
pub fn install_realtime_clock(rtc: Session<rtc::Protocol>) -> Result<(), RealtimeClockError> {
    if runtime::realtime().is_some() {
        return Err(RealtimeClockError::AlreadyInstalled);
    }

    let seconds = rtc
        .client()
        .now()
        .map_err(|_| RealtimeClockError::Unavailable)?;

    if runtime::install_realtime(seconds) {
        Ok(())
    } else {
        Err(RealtimeClockError::AlreadyInstalled)
    }
}

pub(crate) fn remaining(clock: Clock, seconds: i64, nanoseconds: i64) -> Result<Duration, Errno> {
    let nanoseconds = subsecond(nanoseconds).ok_or(Errno::InvalidArgument)?;
    let now = clock.now().ok_or(Errno::InvalidArgument)?;
    let Ok(seconds) = u64::try_from(seconds) else {
        return Ok(Duration::ZERO);
    };

    Ok(Duration::new(seconds, nanoseconds).saturating_sub(now))
}

fn subsecond(nanoseconds: i64) -> Option<u32> {
    u32::try_from(nanoseconds)
        .ok()
        .filter(|nanoseconds| *nanoseconds < NS_PER_SEC)
}

#[allow(unsafe_code)]
#[unsafe(no_mangle)]
extern "C" fn c_library_clock_read(clock: ClockId, time: &mut MaybeUninit<Time>) -> c_int {
    let Some(clock) = Clock::from_id(clock) else {
        return fail(Errno::InvalidArgument, -1);
    };

    let Some(now) = clock.now() else {
        return fail(Errno::NotConnected, -1);
    };

    time.write(Time::of(now));
    0
}

#[allow(unsafe_code)]
#[unsafe(no_mangle)]
extern "C" fn c_library_clock_resolution(clock: ClockId, time: &mut MaybeUninit<Time>) -> c_int {
    if Clock::from_id(clock).is_none() {
        return fail(Errno::InvalidArgument, -1);
    }

    time.write(Time::of(Instant::resolution()));
    0
}

#[allow(unsafe_code)]
#[unsafe(no_mangle)]
extern "C" fn c_library_sleep(seconds: i64, nanoseconds: i64) -> c_int {
    let (Ok(seconds), Some(nanoseconds)) = (u64::try_from(seconds), subsecond(nanoseconds)) else {
        return fail(Errno::InvalidArgument, -1);
    };

    runtime::sleep(Duration::new(seconds, nanoseconds));
    0
}
