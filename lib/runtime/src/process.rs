//! Завершение процесса и его метеринг-ресурс.

use core::sync::atomic::{AtomicU32, Ordering};

use syscall::{Handle, kind};

use crate::{restore, svc};

static METERING: AtomicU32 = AtomicU32::new(0);

static BOOTSTRAP: AtomicU32 = AtomicU32::new(0);

/// Принимает стартовый handle, который ядро кладёт в `x0` точки входа.
///
/// Вызывается точкой входа программы один раз до `main`.
pub fn start(bootstrap: u32) {
    BOOTSTRAP.store(bootstrap, Ordering::Release);
}

/// Отдаёт стартовый handle процесса первому вызову. Дальше `None`.
///
/// По этому handle компонент композиции принимает стартовый набор capability, а корень
/// композиции обращается к ядру. Владение уходит вызывающему, и выдача одна.
pub fn take_bootstrap() -> Option<Handle<kind::Any>> {
    restore(BOOTSTRAP.swap(0, Ordering::AcqRel))
}

/// Завершает процесс со всеми его потоками. Не возвращается.
pub fn exit(code: i32) -> ! {
    svc::process_exit(u64::from(code.cast_unsigned()))
}

/// Кэшированный handle метеринг-ресурса процесса.
pub fn metering_handle() -> Option<Handle<kind::Resource>> {
    if let Some(cached) = restore(METERING.load(Ordering::Acquire)) {
        return Some(cached);
    }

    // `ProcessResourceSelf` ставит свежий handle на каждый вызов. Кэш держит первый,
    // проигравшая гонку сторона закрывает свой дубль.
    let fresh = svc::process_resource_self().ok()?;
    match METERING.compare_exchange(0, fresh.raw(), Ordering::AcqRel, Ordering::Acquire) {
        Ok(_) => Some(fresh),
        Err(existing) => {
            svc::handle_close(fresh);
            restore(existing)
        }
    }
}
