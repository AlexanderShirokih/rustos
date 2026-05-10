//! Сигнальное состояние kernel-объекта и список ожидающих.
//!
//! Любой waiter представлен `Arc<dyn Waker>`; в production-сборке
//! конкретная реализация припаркует поток в `WaitQueue`, в тестах
//! используется `MockWaker`, фиксирующий факт пробуждения и
//! последнее наблюдённое состояние сигналов.

use alloc::{sync::Arc, vec::Vec};
use core::sync::atomic::{AtomicU32, Ordering};

use collections::{LockCell, MutexCell};

use super::runtime::{KernelRuntime, ParkState, WaitToken};

/// Объект, который хочет быть разбуженным при изменении сигналов KO.
pub trait Waker: Send + Sync {
    /// Вызывается, когда `signals` после обновления стал содержать хотя
    /// бы один бит из маски, переданной при регистрации.
    fn wake(&self, observed: u32);
}

/// Цель для cancel-on-handle-close. Идемпотентна; в гонке с signal/
/// timeout побеждает один из них.
pub trait CancelTarget: Send + Sync {
    fn cancel(&self);
}

/// Источник сигналов: KO, на чьи биты можно подписаться через
/// [`SignalState::register_waiter`].
///
/// Существует, чтобы хранить ссылку на target подписки (`Weak<dyn
/// SignalSource>`) без знания конкретного KO-варианта. Реализуется
/// каждым signalable-KO; на несигналуемых KO (Memory/PhysicalResource)
/// не реализуется намеренно.
pub trait SignalSource: Send + Sync {
    fn signals(&self) -> &SignalState;
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

    /// Регистрирует waiter без fast-path-wake: даже если требуемые
    /// биты уже выставлены, `wake` синхронно не вызывается; waker
    /// ждёт следующего `signal()`.
    pub fn register_waiter_silent(&self, mask: u32, waker: Arc<dyn Waker>) {
        self.waiters.with_lock(|wl| {
            wl.entries.push(WaiterEntry { mask, waker });
        });
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

/// Waker запаркованного потока. Latch'инг `observed`/`winner_index`
/// до CAS на `state` + Release/Acquire-пара на `state` гарантируют:
/// reader, увидев `SIGNALED`, читает именно те значения, что были
/// записаны signal-стороной.
pub(super) struct ParkWaker {
    state: AtomicU32,
    observed: AtomicU32,
    winner_index: AtomicU32,
    runtime: Arc<dyn KernelRuntime>,
    token: WaitToken,
}

impl ParkWaker {
    pub(super) const NO_WINNER: u32 = u32::MAX;

    pub(super) fn new(runtime: Arc<dyn KernelRuntime>, token: WaitToken) -> Self {
        Self {
            state: AtomicU32::new(ParkState::REGISTERED),
            observed: AtomicU32::new(0),
            winner_index: AtomicU32::new(Self::NO_WINNER),
            runtime,
            token,
        }
    }

    pub(super) fn state(&self) -> &AtomicU32 {
        &self.state
    }

    pub(super) fn observed(&self) -> u32 {
        self.observed.load(Ordering::Acquire)
    }

    pub(super) fn winner_index(&self) -> u32 {
        self.winner_index.load(Ordering::Acquire)
    }

    fn signal(&self, index: u32, observed: u32) {
        self.observed.store(observed, Ordering::Release);
        self.winner_index.store(index, Ordering::Release);
        if self
            .state
            .compare_exchange(
                ParkState::REGISTERED,
                ParkState::SIGNALED,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
        {
            self.runtime.unblock(self.token);
        }
    }

    pub(super) fn claim_timeout(&self) -> bool {
        self.state
            .compare_exchange(
                ParkState::REGISTERED,
                ParkState::TIMEOUT,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
    }

    pub(super) fn claim_cancel(&self) -> bool {
        self.state
            .compare_exchange(
                ParkState::REGISTERED,
                ParkState::CANCELED,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
    }
}

impl CancelTarget for ParkWaker {
    fn cancel(&self) {
        if self.claim_cancel() {
            self.runtime.unblock(self.token);
        }
    }
}

impl Waker for ParkWaker {
    fn wake(&self, observed: u32) {
        self.signal(0, observed);
    }
}

/// Адаптер `Waker` поверх [`ParkWaker`] для `object_wait_many`:
/// латчит свой `index` в shared waker'е при срабатывании сигнала.
pub(super) struct IndexedWaker {
    inner: Arc<ParkWaker>,
    index: u32,
}

impl IndexedWaker {
    pub(super) fn new(inner: Arc<ParkWaker>, index: u32) -> Self {
        Self { inner, index }
    }
}

impl Waker for IndexedWaker {
    fn wake(&self, observed: u32) {
        self.inner.signal(self.index, observed);
    }
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
    use core::num::NonZeroU64;

    use super::{
        super::{
            errors::{IpcError, SpawnError},
            handle_table::HandleTable,
            process::ProcessObject,
            runtime::{KernelRuntime, UserThreadEntry, WaitToken},
            thread::ThreadObject,
        },
        *,
    };

    const BIT0: u32 = 1 << 0;
    const BIT1: u32 = 1 << 1;

    struct StubRuntime {
        unblocks: core::sync::atomic::AtomicU64,
    }

    impl StubRuntime {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                unblocks: core::sync::atomic::AtomicU64::new(0),
            })
        }

        fn unblock_count(&self) -> u64 {
            self.unblocks.load(Ordering::Acquire)
        }
    }

    impl KernelRuntime for StubRuntime {
        fn current_wait_token(&self) -> WaitToken {
            WaitToken::new(NonZeroU64::new(1).unwrap())
        }
        fn current_handle_table(&self) -> Option<Arc<collections::MutexCell<HandleTable>>> {
            None
        }
        fn current_thread_object(&self) -> Option<Arc<ThreadObject>> {
            None
        }
        fn current_process_object(&self) -> Option<Arc<ProcessObject>> {
            None
        }
        fn exit_current_thread(&self, _exit_code: i32) -> ! {
            unreachable!("not used in wait.rs tests")
        }
        fn block_current_until(&self, _ready_flag: &AtomicU32, _timeout_ns: Option<u64>) {}
        fn unblock(&self, _token: WaitToken) {
            self.unblocks.fetch_add(1, Ordering::AcqRel);
        }
        fn create_empty_process(
            &self,
            _name: &'static str,
        ) -> Result<Arc<ProcessObject>, SpawnError> {
            Err(SpawnError::NoFreeProcessSlots)
        }
        fn create_user_thread(
            &self,
            _process: &Arc<ProcessObject>,
            _entry: UserThreadEntry,
        ) -> Result<Arc<ThreadObject>, SpawnError> {
            Err(SpawnError::NoFreeThreadSlots)
        }
        fn terminate_thread(
            &self,
            _thread: &Arc<ThreadObject>,
            _exit_code: i32,
        ) -> Result<(), IpcError> {
            Ok(())
        }
        fn terminate_process(
            &self,
            _process: &Arc<ProcessObject>,
            _exit_code: i32,
        ) -> Result<(), IpcError> {
            Ok(())
        }
    }

    fn make_waker(runtime: Arc<dyn KernelRuntime>) -> Arc<ParkWaker> {
        Arc::new(ParkWaker::new(
            runtime,
            WaitToken::new(NonZeroU64::new(1).unwrap()),
        ))
    }

    #[test]
    fn park_waker_cancel_transitions_to_canceled_and_unblocks() {
        let rt = StubRuntime::new();
        let waker = make_waker(rt.clone() as Arc<dyn KernelRuntime>);
        assert_eq!(waker.state().load(Ordering::Acquire), ParkState::REGISTERED);

        CancelTarget::cancel(waker.as_ref());
        assert_eq!(waker.state().load(Ordering::Acquire), ParkState::CANCELED);
        assert_eq!(rt.unblock_count(), 1);

        CancelTarget::cancel(waker.as_ref());
        assert_eq!(rt.unblock_count(), 1);
    }

    #[test]
    fn park_waker_signal_wins_over_cancel() {
        let rt = StubRuntime::new();
        let waker = make_waker(rt.clone() as Arc<dyn KernelRuntime>);

        Waker::wake(waker.as_ref(), BIT0);
        assert_eq!(waker.state().load(Ordering::Acquire), ParkState::SIGNALED);
        assert_eq!(rt.unblock_count(), 1);

        CancelTarget::cancel(waker.as_ref());
        assert_eq!(waker.state().load(Ordering::Acquire), ParkState::SIGNALED);
        assert_eq!(rt.unblock_count(), 1);
    }

    #[test]
    fn park_waker_timeout_blocks_cancel_and_signal() {
        let rt = StubRuntime::new();
        let waker = make_waker(rt.clone() as Arc<dyn KernelRuntime>);

        assert!(waker.claim_timeout());
        assert_eq!(waker.state().load(Ordering::Acquire), ParkState::TIMEOUT);

        CancelTarget::cancel(waker.as_ref());
        Waker::wake(waker.as_ref(), BIT0);
        assert_eq!(waker.state().load(Ordering::Acquire), ParkState::TIMEOUT);
        assert_eq!(rt.unblock_count(), 0);
    }

    #[test]
    fn park_waker_cancel_blocks_subsequent_timeout() {
        let rt = StubRuntime::new();
        let waker = make_waker(rt.clone() as Arc<dyn KernelRuntime>);

        CancelTarget::cancel(waker.as_ref());
        assert!(!waker.claim_timeout());
        Waker::wake(waker.as_ref(), BIT0);
        assert_eq!(waker.state().load(Ordering::Acquire), ParkState::CANCELED);
        assert_eq!(rt.unblock_count(), 1);
    }

    #[test]
    fn indexed_waker_latches_index_and_observed() {
        let rt = StubRuntime::new();
        let waker = make_waker(rt.clone() as Arc<dyn KernelRuntime>);
        assert_eq!(waker.winner_index(), ParkWaker::NO_WINNER);

        let indexed = IndexedWaker::new(waker.clone(), 7);
        Waker::wake(&indexed, BIT0 | BIT1);

        assert_eq!(waker.state().load(Ordering::Acquire), ParkState::SIGNALED);
        assert_eq!(waker.winner_index(), 7);
        assert_eq!(waker.observed(), BIT0 | BIT1);
        assert_eq!(rt.unblock_count(), 1);
    }

    #[test]
    fn indexed_waker_first_signal_wins_subsequent_no_op_unblock() {
        let rt = StubRuntime::new();
        let waker = make_waker(rt.clone() as Arc<dyn KernelRuntime>);

        let a = IndexedWaker::new(waker.clone(), 0);
        let b = IndexedWaker::new(waker.clone(), 1);

        Waker::wake(&a, BIT0);
        Waker::wake(&b, BIT1);

        assert_eq!(waker.state().load(Ordering::Acquire), ParkState::SIGNALED);
        assert_eq!(rt.unblock_count(), 1);
        let idx = waker.winner_index();
        assert!(idx == 0 || idx == 1, "unexpected winner_index {idx}");
        let observed = waker.observed();
        assert!(
            observed == BIT0 || observed == BIT1,
            "unexpected observed {observed:#x}",
        );
    }

    #[test]
    fn indexed_waker_no_op_after_cancel() {
        let rt = StubRuntime::new();
        let waker = make_waker(rt.clone() as Arc<dyn KernelRuntime>);

        CancelTarget::cancel(waker.as_ref());
        let indexed = IndexedWaker::new(waker.clone(), 3);
        Waker::wake(&indexed, BIT0);

        assert_eq!(waker.state().load(Ordering::Acquire), ParkState::CANCELED);
        assert_eq!(rt.unblock_count(), 1);
    }

    /// Regression guard: `observed` после wake не сбивается тем,
    /// что SignalState успел снять биты. Без этого `object_wait_one`
    /// возвращал бы Timeout на успешно отработавший signal.
    #[test]
    fn park_waker_observed_survives_signal_state_clear() {
        let s = SignalState::new(0);
        let rt = StubRuntime::new();
        let waker = make_waker(rt.clone() as Arc<dyn KernelRuntime>);
        let waker_dyn: Arc<dyn Waker> = waker.clone();

        s.register_waiter(BIT0, waker_dyn);
        s.signal(BIT0, 0);
        s.signal(0, BIT0);
        assert_eq!(s.peek() & BIT0, 0);

        assert_eq!(waker.state().load(Ordering::Acquire), ParkState::SIGNALED);
        assert_eq!(waker.observed() & BIT0, BIT0);
    }

    #[test]
    fn indexed_waker_observed_and_index_survive_signal_state_clear() {
        let s = SignalState::new(0);
        let rt = StubRuntime::new();
        let waker = make_waker(rt.clone() as Arc<dyn KernelRuntime>);
        let indexed: Arc<dyn Waker> = Arc::new(IndexedWaker::new(waker.clone(), 5));

        s.register_waiter(BIT0, indexed);
        s.signal(BIT0, 0);
        s.signal(0, BIT0);
        assert_eq!(s.peek() & BIT0, 0);

        assert_eq!(waker.state().load(Ordering::Acquire), ParkState::SIGNALED);
        assert_eq!(waker.observed() & BIT0, BIT0);
        assert_eq!(waker.winner_index(), 5);
    }

    #[test]
    fn indexed_waker_resolves_correct_index_across_signal_states() {
        let s0 = SignalState::new(0);
        let s1 = SignalState::new(0);
        let s2 = SignalState::new(0);

        let rt = StubRuntime::new();
        let waker = make_waker(rt.clone() as Arc<dyn KernelRuntime>);
        let i0: Arc<dyn Waker> = Arc::new(IndexedWaker::new(waker.clone(), 0));
        let i1: Arc<dyn Waker> = Arc::new(IndexedWaker::new(waker.clone(), 1));
        let i2: Arc<dyn Waker> = Arc::new(IndexedWaker::new(waker.clone(), 2));

        s0.register_waiter(BIT0, i0);
        s1.register_waiter(BIT0, i1);
        s2.register_waiter(BIT0, i2);

        // Сигнал прилетает только в SignalState с индексом 1.
        s1.signal(BIT0, 0);
        s1.signal(0, BIT0);

        assert_eq!(waker.state().load(Ordering::Acquire), ParkState::SIGNALED);
        assert_eq!(waker.winner_index(), 1);
        assert_eq!(waker.observed() & BIT0, BIT0);
    }

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
