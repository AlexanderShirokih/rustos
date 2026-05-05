#![allow(unsafe_code)]

use core::{cell::UnsafeCell, marker::PhantomData};

/// Абстракция над ячейкой с эксклюзивным доступом.
///
/// Позволяет выбирать стратегию синхронизации: без блокировок
/// для однопоточного кода или со спинлоком для многопоточного.
pub trait LockCell<T> {
    /// Создаёт ячейку с начальным значением.
    fn new(value: T) -> Self;

    /// Извлекает значение, потребляя ячейку.
    fn into_inner(self) -> T;

    /// Выполняет замыкание с эксклюзивным доступом к значению.
    fn with_lock<R>(&self, f: impl FnOnce(&mut T) -> R) -> R;
}

/// Стратегия критической секции для защиты от прерываний.
///
/// Перед захватом спинлока входим в критическую секцию, при выходе - восстанавливаем предыдущее состояние.
pub trait CriticalSection {
    /// RAII-guard, восстанавливающий состояние при Drop.
    type Guard;

    /// Вход в критическую секцию.
    fn enter() -> Self::Guard;
}

/// No-op критическая секция.
pub struct NoCriticalSection;

impl CriticalSection for NoCriticalSection {
    type Guard = ();
    fn enter() -> Self::Guard {}
}

/// Критическая секция через маскировку IRQ в DAIF (AArch64).
#[cfg(target_arch = "aarch64")]
pub struct DaifCriticalSection;

/// RAII-guard для восстановления DAIF при выходе из критической секции.
#[cfg(target_arch = "aarch64")]
pub struct DaifGuard(u64);

#[cfg(target_arch = "aarch64")]
impl CriticalSection for DaifCriticalSection {
    type Guard = DaifGuard;

    fn enter() -> DaifGuard {
        let daif: u64;
        // SAFETY: Чтение DAIF - безопасная операция, не меняющая состояние процессора.
        unsafe {
            core::arch::asm!("mrs {}, daif", out(reg) daif, options(nomem, nostack));
        }
        // SAFETY: Маскируем IRQ для предотвращения deadlock при захвате спинлока.
        unsafe {
            core::arch::asm!("msr daifset, #0b0010", options(nostack, preserves_flags));
        }
        DaifGuard(daif)
    }
}

#[cfg(target_arch = "aarch64")]
impl Drop for DaifGuard {
    fn drop(&mut self) {
        if self.0 & (1 << 7) == 0 {
            // SAFETY: Восстанавливаем исходное состояние IRQ-маски -
            // IRQ были разрешены до входа в секцию.
            unsafe {
                core::arch::asm!("msr daifclr, #0b0010", options(nostack, preserves_flags));
            }
        }
    }
}

/// Платформенная критическая секция по умолчанию.
#[cfg(target_arch = "aarch64")]
pub type DefaultCriticalSection = DaifCriticalSection;

/// Платформенная критическая секция по умолчанию.
#[cfg(not(target_arch = "aarch64"))]
pub type DefaultCriticalSection = NoCriticalSection;

/// Ячейка без синхронизации для однопоточного кода.
///
/// # Safety
///
/// Вызывающий код должен гарантировать отсутствие
/// одновременного доступа из разных потоков.
pub struct NoLockCell<T> {
    /// Внутреннее значение.
    inner: UnsafeCell<T>,
}

impl<T> NoLockCell<T> {
    pub const fn new(value: T) -> Self {
        Self {
            inner: UnsafeCell::new(value),
        }
    }
}

impl<T> LockCell<T> for NoLockCell<T> {
    fn new(value: T) -> Self {
        Self::new(value)
    }

    fn into_inner(self) -> T {
        self.inner.into_inner()
    }

    fn with_lock<R>(&self, lock: impl FnOnce(&mut T) -> R) -> R {
        let ptr = self.inner.get();

        // SAFETY: Вызывающий код гарантирует однопоточный доступ
        lock(unsafe { &mut *ptr })
    }
}

/// Ячейка с защитой через спинлок.
///
/// Перед захватом мьютекса входит в критическую секцию `CS`
/// для предотвращения deadlock при вложенных прерываниях.
pub struct MutexCell<T, CS: CriticalSection = DefaultCriticalSection> {
    /// Мьютекс для синхронизации доступа.
    mutex: spin::Mutex<T>,
    _cs: PhantomData<CS>,
}

impl<T, CS: CriticalSection> MutexCell<T, CS> {
    pub const fn new(value: T) -> Self {
        Self {
            mutex: spin::Mutex::new(value),
            _cs: PhantomData,
        }
    }
}

impl<T, CS: CriticalSection> LockCell<T> for MutexCell<T, CS> {
    fn new(value: T) -> Self {
        Self::new(value)
    }

    fn into_inner(self) -> T {
        self.mutex.into_inner()
    }

    fn with_lock<R>(&self, f: impl FnOnce(&mut T) -> R) -> R {
        let _cs = CS::enter();
        let mut guard = self.mutex.lock();
        f(&mut *guard)
    }
}
