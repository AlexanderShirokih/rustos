//! Запись факта SVC из EL0 для интеграционного теста userspace-входа.
//!
//! Платформенный тест запускает payload из EL0, который делает
//! `svc #TestEl0Probe`. Handler в [`crate::syscall::dispatch`] зовёт
//! [`record`] с первым аргументом системного вызова и [`Origin`] фрейма,
//! потом завершает thread. Тестовый kernel-thread читает зафиксированное
//! состояние через [`peek`] и сверяет с ожидаемыми значениями.

use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use crate::syscall::Origin;

static RECORDED_ARG0: AtomicU64 = AtomicU64::new(0);
/// `true` - `Origin::User`, `false` - `Origin::Kernel`. Пишется только
/// после `RECORDED` стало `true` (release-store снизу гарантирует
/// видимость).
static RECORDED_ORIGIN_USER: AtomicBool = AtomicBool::new(false);
static RECORDED: AtomicBool = AtomicBool::new(false);

/// Фиксирует факт вызова `TestEl0Probe`. Вызывается из syscall-handler'а
/// в произвольном thread'е (включая EL0-source); реализация lock-free.
pub fn record(arg0: u64, origin: Origin) {
    RECORDED_ARG0.store(arg0, Ordering::Relaxed);
    RECORDED_ORIGIN_USER.store(matches!(origin, Origin::User), Ordering::Relaxed);
    // release-store: после установки `RECORDED` reader через acquire-load
    // увидит обновлённые `RECORDED_X0` / `RECORDED_ORIGIN_USER`.
    RECORDED.store(true, Ordering::Release);
}

/// Возвращает `Some((arg0, is_user))` если probe был зафиксирован, иначе `None`.
/// Не сбрасывает состояние.
pub fn peek() -> Option<(u64, bool)> {
    if RECORDED.load(Ordering::Acquire) {
        Some((
            RECORDED_ARG0.load(Ordering::Relaxed),
            RECORDED_ORIGIN_USER.load(Ordering::Relaxed),
        ))
    } else {
        None
    }
}

/// Сбрасывает состояние в "не зафиксировано" - для повторного запуска
/// теста в одном QEMU-сеансе.
pub fn reset() {
    RECORDED.store(false, Ordering::Release);
}
