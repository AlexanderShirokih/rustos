//! Сигнальное состояние kernel-объекта и список ожидающих.
//!
//! Phase 2: каркас без интеграции с планировщиком. Любой waiter
//! представлен `Arc<dyn Waker>`; в production-сборке конкретная
//! реализация припаркует поток в `WaitQueue` (Phase 3), в тестах
//! используется `MockWaker`, фиксирующий факт пробуждения и
//! последнее наблюдённое состояние сигналов.

use alloc::{sync::Arc, vec::Vec};
use core::sync::atomic::{AtomicU32, Ordering};

use collections::{LockCell, MutexCell};

/// Объект, который хочет быть разбуженным при изменении сигналов KO.
pub trait Waker: Send + Sync {
    /// Вызывается, когда `signals` после обновления стал содержать хотя
    /// бы один бит из маски, переданной при регистрации.
    fn wake(&self, observed: u32);
}

/// Сигнальное состояние KO: атомарные битовые флаги + список ожидающих.
///
/// `peek` лочно-свободен; `signal`/`register_waiter` берут короткий
/// мьютекс waiters'ов, что одновременно сериализует обновление битов
/// и гарантирует, что регистрация waiter'а не "проскочит" мимо
/// одновременной публикации сигнала.
pub struct SignalState {
    bits: AtomicU32,
    waiters: MutexCell<WaiterList>,
}

impl SignalState {
    pub fn new(initial: u32) -> Self {
        Self {
            bits: AtomicU32::new(initial),
            waiters: MutexCell::new(WaiterList::new()),
        }
    }

    pub fn peek(&self) -> u32 {
        self.bits.load(Ordering::Acquire)
    }

    /// Атомарно применяет `set`/`clear` к текущим битам и будит
    /// зарегистрированных waiter'ов, чьи маски пересекаются
    /// с новым набором сигналов.
    pub fn signal(&self, set: u32, clear: u32) {
        let mut to_wake: Vec<(Arc<dyn Waker>, u32)> = Vec::new();

        self.waiters.with_lock(|wl| {
            let prev = self.bits.load(Ordering::Relaxed);
            let new = (prev & !clear) | set;
            self.bits.store(new, Ordering::Release);

            // Будим waiter'ов, чья маска пересекается с новым состоянием.
            // Делаем drain: проснулся - забываем про waiter; не проснулся -
            // оставляем в списке.
            wl.entries.retain(|entry| {
                if entry.mask & new != 0 {
                    to_wake.push((entry.waker.clone(), new));
                    false
                } else {
                    true
                }
            });
        });

        // Будим вне локов, чтобы waker мог свободно брать другие локи.
        for (waker, observed) in to_wake {
            waker.wake(observed);
        }
    }

    /// Регистрирует waiter. Если требуемые сигналы уже присутствуют,
    /// waiter вызывается немедленно (и не добавляется в список).
    pub fn register_waiter(&self, mask: u32, waker: Arc<dyn Waker>) {
        let mut waker_slot = Some(waker);
        let observed = self.waiters.with_lock(|wl| {
            let current = self.bits.load(Ordering::Acquire);
            if mask & current != 0 {
                Some(current)
            } else {
                let waker = waker_slot.take().expect("waker still present");
                wl.entries.push(WaiterEntry { mask, waker });
                None
            }
        });

        if let Some(observed) = observed {
            // Будим вне лока. waker не был перемещён в список выше.
            let waker = waker_slot.take().expect("waker not pushed");
            waker.wake(observed);
        }
    }
}

impl core::fmt::Debug for SignalState {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("SignalState")
            .field("bits", &self.peek())
            .finish()
    }
}

/// Внутренний список зарегистрированных waiter'ов.
pub(super) struct WaiterList {
    entries: Vec<WaiterEntry>,
}

impl WaiterList {
    pub(super) fn new() -> Self {
        Self {
            entries: Vec::new(),
        }
    }
}

struct WaiterEntry {
    mask: u32,
    waker: Arc<dyn Waker>,
}

/// Тестовый waker: фиксирует факт пробуждения и последний наблюдённый набор сигналов.
#[cfg(test)]
pub(super) struct MockWaker {
    woken: core::sync::atomic::AtomicBool,
    observed: AtomicU32,
}

#[cfg(test)]
impl MockWaker {
    pub(super) fn new() -> Arc<Self> {
        Arc::new(Self {
            woken: core::sync::atomic::AtomicBool::new(false),
            observed: AtomicU32::new(0),
        })
    }

    pub(super) fn was_woken(&self) -> bool {
        self.woken.load(Ordering::Acquire)
    }

    pub(super) fn observed(&self) -> u32 {
        self.observed.load(Ordering::Acquire)
    }
}

#[cfg(test)]
impl Waker for MockWaker {
    fn wake(&self, observed: u32) {
        self.observed.store(observed, Ordering::Release);
        self.woken.store(true, Ordering::Release);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BIT0: u32 = 1 << 0;
    const BIT1: u32 = 1 << 1;

    #[test]
    fn signal_then_register_wakes_immediately() {
        let s = SignalState::new(0);
        s.signal(BIT0, 0);

        let w = MockWaker::new();
        s.register_waiter(BIT0, w.clone());
        assert!(w.was_woken());
        assert_eq!(w.observed() & BIT0, BIT0);
    }

    #[test]
    fn register_then_signal_wakes() {
        let s = SignalState::new(0);
        let w = MockWaker::new();
        s.register_waiter(BIT0, w.clone());
        assert!(!w.was_woken());

        s.signal(BIT0, 0);
        assert!(w.was_woken());
    }

    #[test]
    fn signal_does_not_wake_unrelated_mask() {
        let s = SignalState::new(0);
        let w = MockWaker::new();
        s.register_waiter(BIT1, w.clone());

        s.signal(BIT0, 0);
        assert!(!w.was_woken());

        s.signal(BIT1, 0);
        assert!(w.was_woken());
    }

    #[test]
    fn clear_does_not_wake() {
        let s = SignalState::new(BIT0);
        let w = MockWaker::new();
        s.register_waiter(BIT1, w.clone());

        s.signal(0, BIT0);
        assert!(!w.was_woken());
        assert_eq!(s.peek(), 0);
    }

    #[test]
    fn waiter_consumed_after_wake() {
        let s = SignalState::new(0);
        let w = MockWaker::new();
        s.register_waiter(BIT0, w.clone());
        s.signal(BIT0, 0);
        assert!(w.was_woken());

        // Сбрасываем bit, повторно сигналим - повторного пробуждения быть не должно
        // (waiter был "потреблён"; новая регистрация требуется).
        let prev = w.was_woken();
        s.signal(0, BIT0);
        s.signal(BIT0, 0);
        assert_eq!(w.was_woken(), prev);
    }

    #[test]
    fn peek_reflects_signal_changes() {
        let s = SignalState::new(0);
        assert_eq!(s.peek(), 0);
        s.signal(BIT0 | BIT1, 0);
        assert_eq!(s.peek(), BIT0 | BIT1);
        s.signal(0, BIT0);
        assert_eq!(s.peek(), BIT1);
    }
}
