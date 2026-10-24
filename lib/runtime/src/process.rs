//! Завершение процесса и его метеринг-ресурс.

use core::{
    fmt::Display,
    num::NonZeroU32,
    panic::PanicInfo,
    sync::atomic::{AtomicPtr, AtomicU32, Ordering},
};

use syscall::{Handle, kind};
use syscall_user::IssuedHandle;

use crate::{restore, svc};

/// Код выхода процесса, завершённого успешно.
pub const SUCCESS_EXIT_CODE: i32 = 0;

/// Код выхода процесса, завершённого отказом.
pub const FAILED_EXIT_CODE: i32 = 1;

static METERING: AtomicU32 = AtomicU32::new(0);

static BOOTSTRAP: AtomicU32 = AtomicU32::new(0);

static PANIC_HOOK: AtomicPtr<()> = AtomicPtr::new(core::ptr::null_mut());

/// Исход программы, который задаёт код выхода процесса.
pub trait Termination {
    /// Сообщает исход и отдаёт код выхода.
    fn report(self) -> i32;
}

/// Код выхода, который программа сама выбирает.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExitCode(i32);

impl ExitCode {
    #[must_use]
    pub const fn new(code: i32) -> Self {
        Self(code)
    }
}

impl From<u32> for ExitCode {
    fn from(code: u32) -> Self {
        Self(code.cast_signed())
    }
}

impl Termination for ExitCode {
    fn report(self) -> i32 {
        self.0
    }
}

/// `Ok` завершает кодом [`SUCCESS_EXIT_CODE`], `Err` пишет в журнал строку `stopped: {error}`
/// и завершает кодом [`FAILED_EXIT_CODE`].
impl<E: Display> Termination for Result<(), E> {
    fn report(self) -> i32 {
        match self {
            Ok(()) => SUCCESS_EXIT_CODE,
            Err(error) => {
                crate::log::fmt(format_args!("stopped: {error}"));
                FAILED_EXIT_CODE
            }
        }
    }
}

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

/// Завершает процесс со всеми его потоками, дождавшись приёма поставленных строк журнала не
/// дольше [`LOG_FLUSH_TIMEOUT`](crate::log::LOG_FLUSH_TIMEOUT). Не возвращается.
pub fn exit(code: i32) -> ! {
    crate::log::settle();
    svc::process_exit(u64::from(code.cast_unsigned()))
}

/// Завершает процесс кодом исхода программы.
pub fn finish<T: Termination>(outcome: T) -> ! {
    exit(outcome.report())
}

/// Заменяет запись паники в журнал вызовом `hook`. Процесс после него завершается кодом
/// [`FAILED_EXIT_CODE`].
pub fn set_panic_hook(hook: fn(&PanicInfo<'_>)) {
    PANIC_HOOK.store(hook as *mut (), Ordering::Release);
}

/// Сообщает о панике и завершает процесс кодом [`FAILED_EXIT_CODE`]. Без hook паника
/// записывается в журнал строкой `panic: {info}`.
pub fn panic(info: &PanicInfo<'_>) -> ! {
    let hook = PANIC_HOOK.load(Ordering::Acquire);
    if hook.is_null() {
        crate::log::fmt(format_args!("panic: {info}"));
    } else {
        // SAFETY: ненулевое значение записал только `set_panic_hook` из указателя на функцию
        // той же сигнатуры.
        let hook = unsafe { core::mem::transmute::<*mut (), fn(&PanicInfo<'_>)>(hook) };
        hook(info);
    }
    exit(FAILED_EXIT_CODE)
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
