//! Общий lifecycle-стейт Process/Thread: exit-код + bound-`Signal`.
//!
//! Завершение наблюдается через [`Signal`], материализуемый по требованию (`termination_signal`): ненаблюдаемый объект не аллоцирует `Signal` вовсе и

use alloc::sync::Arc;
use core::sync::atomic::{AtomicBool, AtomicI32, Ordering};

use collections::{LockCell, MutexCell};

use super::signal::{SIGNALED, Signal};

pub(crate) struct TerminationState {
    terminated: AtomicBool,
    exit_code: AtomicI32,
    signal: MutexCell<Option<Arc<Signal>>>,
}

impl TerminationState {
    pub(crate) fn new() -> Self {
        Self {
            terminated: AtomicBool::new(false),
            exit_code: AtomicI32::new(0),
            signal: MutexCell::new(None),
        }
    }

    /// Идемпотентно публикует `code`, помечает объект завершённым и, если
    /// bound-`Signal` уже материализован, поднимает на нём `SIGNALED`.
    pub(crate) fn signal_terminated(&self, code: i32) {
        let to_signal = self.signal.with_lock(|slot| {
            if self.terminated.load(Ordering::Acquire) {
                return None;
            }
            self.exit_code.store(code, Ordering::Release);
            self.terminated.store(true, Ordering::Release);
            slot.clone()
        });

        if let Some(signal) = to_signal {
            signal.signal(SIGNALED, 0);
        }
    }

    /// Завершён ли объект.
    pub(crate) fn terminated(&self) -> bool {
        self.terminated.load(Ordering::Acquire)
    }

    /// Финальный exit-код. До завершения возвращает 0.
    pub(crate) fn exit_code(&self) -> i32 {
        self.exit_code.load(Ordering::Acquire)
    }

    /// Возвращает bound-`Signal` термнинации, создавая его лениво при первом
    /// вызове. Если объект уже завершён, свежесозданный `Signal` сразу несёт
    /// `SIGNALED` (наблюдение-после-выхода). Повторные вызовы отдают тот же.
    pub(crate) fn termination_signal(&self) -> Arc<Signal> {
        self.signal.with_lock(|slot| {
            if let Some(signal) = slot.as_ref() {
                return signal.clone();
            }
            let initial = if self.terminated.load(Ordering::Acquire) {
                SIGNALED
            } else {
                0
            };
            let signal = Arc::new(Signal::with_bits(initial));
            *slot = Some(signal.clone());
            signal
        })
    }
}
