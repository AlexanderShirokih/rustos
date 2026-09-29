//! Завершение процесса и его метеринг-ресурс.

use core::{
    num::NonZeroU32,
    sync::atomic::{AtomicU32, Ordering},
};

use syscall::{Handle, kind};
use syscall_user::IssuedHandle;

use crate::{restore, svc};

static METERING: AtomicU32 = AtomicU32::new(0);

static BOOTSTRAP: AtomicU32 = AtomicU32::new(0);

/// Принимает стартовый handle, который ядро кладёт в `x0` точки входа.
///
/// # Safety
///
/// `bootstrap` получен из `x0` точки входа процесса, и вызов в процессе один.
pub unsafe fn start(bootstrap: u32) {
    BOOTSTRAP.store(bootstrap, Ordering::Release);
}

/// Отдаёт владельца стартового handle процесса первому вызову. Дальше `None`.
///
/// По этому handle компонент композиции принимает стартовый набор capability, а корень
/// композиции обращается к ядру.
pub fn take_bootstrap() -> Option<IssuedHandle<kind::Any>> {
    let raw = NonZeroU32::new(BOOTSTRAP.swap(0, Ordering::AcqRel))?;
    // SAFETY: номер записал `start` из `x0` точки входа, и обмен на ноль отдаёт его один раз.
    Some(unsafe { IssuedHandle::adopt(raw) })
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

    // `ProcessResourceSelf` ставит свежий handle на каждый вызов. Кэш держит номер первого до
    // завершения процесса, а дубль проигравшей гонку стороны закрывает его владелец.
    let fresh = svc::process_resource_self().ok()?;
    match METERING.compare_exchange(0, fresh.as_raw().raw(), Ordering::AcqRel, Ordering::Acquire) {
        Ok(_) => Some(fresh.leak()),
        Err(existing) => restore(existing),
    }
}
