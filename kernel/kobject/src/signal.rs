//! `Signal` kernel object: сигнальный примитив ядра.
//!
//! Слово атомарных бит 0..=31 + список ожидающих. Сам по себе сигнальный
//! примитив; также служит ответным каналом в RPC-паттерне
//! «request -> handle на Signal -> ждать `SIGNALED`» и ленивым bound-сигналом
//! термнинации Process/Thread (см. [`termination`](super::termination)).
//!
//! `peek` лочно-свободен; `signal`/`register_waiter` берут короткий мьютекс
//! waiters'ов, что одновременно сериализует обновление битов и гарантирует,
//! что регистрация waiter'а не «проскочит» мимо одновременной публикации сигнала.

use alloc::{sync::Arc, vec::Vec};
use core::sync::atomic::{AtomicU32, Ordering};

use collections::{LockCell, MutexCell};
pub use syscall::SIGNALED;

use super::wait::Waker;

/// Синальный примитив ядра.
pub struct Signal {
    bits: AtomicU32,
    waiters: MutexCell<WaiterList>,
}

impl Signal {
    /// Создаёт новый `Signal` с очищенными сигналами.
    pub fn new() -> Arc<Self> {
        Arc::new(Self::with_bits(0))
    }

    /// Создаёт `Signal` с заранее выставленными битами (для пре-сигнала
    /// уже-наступившего события, напр. термнинации, см. [`super::termination`]).
    pub(crate) fn with_bits(initial: u32) -> Self {
        Self {
            bits: AtomicU32::new(initial),
            waiters: MutexCell::new(WaiterList::new()),
        }
    }

    /// Текущий снимок сигналов.
    pub fn peek(&self) -> u32 {
        self.bits.load(Ordering::Acquire)
    }

    /// Атомарно применяет `set`/`clear` к текущим битам и будит
    /// зарегистрированных waiters, чьи маски пересекаются с новым набором сигналов.
    pub fn signal(&self, set: u32, clear: u32) {
        self.signal_n(set, clear, usize::MAX);
    }

    /// Как [`signal`](Self::signal), но будит не более `max_wake`
    /// пересекающихся waiters в порядке их регистрации.
    /// Биты выставляются всегда, независимо от `max_wake` и числа
    /// разбуженных; разбуженные снимаются из списка, остальные остаются.
    pub fn signal_n(&self, set: u32, clear: u32, max_wake: usize) {
        let mut to_wake: Vec<(Arc<dyn Waker>, u32)> = Vec::new();

        self.waiters.with_lock(|wl| {
            let prev = self.bits.load(Ordering::Relaxed);
            let new = (prev & !clear) | set;
            self.bits.store(new, Ordering::Release);

            wl.entries.retain(|entry| {
                if to_wake.len() < max_wake && entry.mask & new != 0 {
                    to_wake.push((entry.waker.clone(), new));
                    false
                } else {
                    true
                }
            });
        });

        for (waker, observed) in to_wake {
            waker.wake(observed);
        }
    }

    /// Регистрирует waiter. Если требуемые сигналы уже присутствуют,
    /// waiter вызывается немедленно.
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
            let waker = waker_slot.take().expect("waker not pushed");
            waker.wake(observed);
        }
    }

    /// Снимает waiter из списка по identity (`Arc::ptr_eq`). Возвращает
    /// `true`, если запись действительно была удалена.
    pub fn remove_waiter(&self, target: &Arc<dyn Waker>) -> bool {
        self.waiters.with_lock(|wl| {
            let initial = wl.entries.len();
            wl.entries
                .retain(|entry| !Arc::ptr_eq(&entry.waker, target));
            wl.entries.len() != initial
        })
    }
}

impl core::fmt::Debug for Signal {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Signal")
            .field("bits", &self.peek())
            .finish()
    }
}

/// Cписок зарегистрированных waiter'ов.
struct WaiterList {
    entries: Vec<WaiterEntry>,
}

impl WaiterList {
    fn new() -> Self {
        Self {
            entries: Vec::new(),
        }
    }
}

struct WaiterEntry {
    mask: u32,
    waker: Arc<dyn Waker>,
}

#[cfg(test)]
mod tests {
    use super::{super::wait::MockWaker, *};

    const BIT0: u32 = 1 << 0;
    const BIT1: u32 = 1 << 1;

    #[test]
    fn signal_wakes_observer() {
        let signal = Signal::new();
        let w = MockWaker::new();
        signal.register_waiter(SIGNALED, w.clone());

        assert!(!w.was_woken());
        signal.signal(SIGNALED, 0);
        assert!(w.was_woken());
        assert_eq!(w.observed() & SIGNALED, SIGNALED);
    }

    #[test]
    fn peek_returns_current_state() {
        let signal = Signal::new();
        assert_eq!(signal.peek(), 0);
        signal.signal(SIGNALED, 0);
        assert_eq!(signal.peek(), SIGNALED);
        signal.signal(0, SIGNALED);
        assert_eq!(signal.peek(), 0);
    }

    #[test]
    fn koid_unique_per_instance() {
        use crate::object::KObject;
        let a = Signal::new();
        let b = Signal::new();
        assert_ne!(KObject::Signal(a).koid(), KObject::Signal(b).koid());
    }

    #[test]
    fn signal_then_register_wakes_immediately() {
        let s = Signal::new();
        s.signal(BIT0, 0);

        let w = MockWaker::new();
        s.register_waiter(BIT0, w.clone());
        assert!(w.was_woken());
        assert_eq!(w.observed() & BIT0, BIT0);
    }

    #[test]
    fn register_then_signal_wakes() {
        let s = Signal::new();
        let w = MockWaker::new();
        s.register_waiter(BIT0, w.clone());
        assert!(!w.was_woken());

        s.signal(BIT0, 0);
        assert!(w.was_woken());
    }

    #[test]
    fn signal_does_not_wake_unrelated_mask() {
        let s = Signal::new();
        let w = MockWaker::new();
        s.register_waiter(BIT1, w.clone());

        s.signal(BIT0, 0);
        assert!(!w.was_woken());

        s.signal(BIT1, 0);
        assert!(w.was_woken());
    }

    #[test]
    fn clear_does_not_wake() {
        let s = Signal::with_bits(BIT0);
        let w = MockWaker::new();
        s.register_waiter(BIT1, w.clone());

        s.signal(0, BIT0);
        assert!(!w.was_woken());
        assert_eq!(s.peek(), 0);
    }

    #[test]
    fn waiter_consumed_after_wake() {
        let s = Signal::new();
        let w = MockWaker::new();
        s.register_waiter(BIT0, w.clone());
        s.signal(BIT0, 0);
        assert!(w.was_woken());

        let prev = w.was_woken();
        s.signal(0, BIT0);
        s.signal(BIT0, 0);
        assert_eq!(w.was_woken(), prev);
    }

    #[test]
    fn peek_reflects_signal_changes() {
        let s = Signal::new();
        assert_eq!(s.peek(), 0);
        s.signal(BIT0 | BIT1, 0);
        assert_eq!(s.peek(), BIT0 | BIT1);
        s.signal(0, BIT0);
        assert_eq!(s.peek(), BIT1);
    }

    #[test]
    fn signal_n_wakes_at_most_n_in_registration_order() {
        let s = Signal::new();
        let w1 = MockWaker::new();
        let w2 = MockWaker::new();
        let w3 = MockWaker::new();
        s.register_waiter(BIT0, w1.clone());
        s.register_waiter(BIT0, w2.clone());
        s.register_waiter(BIT0, w3.clone());

        s.signal_n(BIT0, 0, 1);
        assert!(w1.was_woken());
        assert!(!w2.was_woken());
        assert!(!w3.was_woken());
    }

    #[test]
    fn signal_n_zero_wakes_none_but_sets_bits() {
        let s = Signal::new();
        let w = MockWaker::new();
        s.register_waiter(BIT0, w.clone());

        s.signal_n(BIT0, 0, 0);
        assert!(!w.was_woken());
        assert_eq!(s.peek() & BIT0, BIT0);
    }

    #[test]
    fn signal_wakes_all_intersecting() {
        let s = Signal::new();
        let w1 = MockWaker::new();
        let w2 = MockWaker::new();
        let w3 = MockWaker::new();
        s.register_waiter(BIT0, w1.clone());
        s.register_waiter(BIT0, w2.clone());
        s.register_waiter(BIT0, w3.clone());

        s.signal(BIT0, 0);
        assert!(w1.was_woken());
        assert!(w2.was_woken());
        assert!(w3.was_woken());
    }
}
