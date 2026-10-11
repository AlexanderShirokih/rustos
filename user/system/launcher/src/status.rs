//! Тексты сведений лаунчера о числе CPU и занятой памяти по снимку `SystemInfo`, о времени с
//! запуска лаунчера и о времени суток по часам реального времени процесса.

use core::time::Duration;
use std::time::{SystemTime, UNIX_EPOCH};

use bootstrap_abi::SystemSnapshot;

const MIB: u64 = 1024 * 1024;
const SECONDS_PER_HOUR: u64 = 3_600;
const SECONDS_PER_DAY: u64 = 86_400;
const WEEKDAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

pub(crate) fn cpu_text(snapshot: SystemSnapshot) -> String {
    snapshot.cpus.to_string()
}

pub(crate) fn memory_text(snapshot: SystemSnapshot) -> String {
    let frame = u64::from(snapshot.frame_size);
    let total = snapshot.managed_frames.saturating_mul(frame) / MIB;
    let used = snapshot
        .managed_frames
        .saturating_sub(snapshot.free_frames)
        .saturating_mul(frame)
        / MIB;
    format!("{used} / {total}")
}

pub(crate) fn uptime_text(elapsed: Duration) -> String {
    let seconds = elapsed.as_secs();
    format!(
        "{:02}:{:02}:{:02}",
        seconds / SECONDS_PER_HOUR,
        seconds % SECONDS_PER_HOUR / 60,
        seconds % 60
    )
}

pub(crate) fn clock_text(now: SystemTime) -> Option<String> {
    let seconds = now.duration_since(UNIX_EPOCH).ok()?.as_secs();
    let days = seconds / SECONDS_PER_DAY;
    let minutes = seconds % SECONDS_PER_DAY / 60;
    let (month, day) = civil_date(days);
    // 1 января 1970 года - четверг.
    let weekday = WEEKDAYS.get(usize::try_from((days + 4) % 7).ok()?)?;
    let month = MONTHS.get(usize::try_from(month.checked_sub(1)?).ok()?)?;
    Some(format!(
        "{weekday} {day} {month}  {:02}:{:02}",
        minutes / 60,
        minutes % 60
    ))
}

fn civil_date(days: u64) -> (u64, u64) {
    // Алгоритм `civil_from_days` Говарда Хиннанта, дни отсчитываются от 1 января 1970 года.
    let day_of_era = days.saturating_add(719_468) % 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_index + 2) / 5 + 1;
    let month = if month_index < 10 {
        month_index + 3
    } else {
        month_index - 9
    };
    (month, day)
}
