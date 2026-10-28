//! Трейт [`Waker`] будит поток, [`Waitable`] называет предмет ожидания, [`CancelTarget`]
//! снимает уже начатое ожидание.

use alloc::sync::Arc;
use core::sync::atomic::{AtomicU32, Ordering};

use super::runtime::{KernelRuntime, ParkState, WaitToken};

/// Объект, разбуждаемый при изменении сигналов capability target.
pub trait Waker: Send + Sync {
    /// Вызывается, когда `signals` после обновления стал содержать хотя
    /// бы один бит из маски, переданной при регистрации.
    fn wake(&self, observed: u32);
}

/// Источник, на котором можно ждать сигналы: абстракция wait-пути над конкретным объектом.
/// [`Signal`](super::signal::Signal) реализует трейт напрямую.
///
/// Составной объект может сам реализовать трейт, маршрутизируя один `waker` на несколько
/// внутренних `Signal` (`mask` остаётся селектором условий). Кардинальность сигналов остаётся
/// скрыта за трейтом, и [`signal_wait_many`](crate::signal_wait_many) не различает конкретных
/// типов `Waitable`.
pub trait Waitable: Send + Sync {
    /// Текущий снимок поднятых сигналов.
    fn peek(&self) -> u32;

    /// Регистрирует `waker` на пересечение с `mask`. Если требуемые сигналы
    /// уже присутствуют, `waker` вызывается немедленно.
    fn register_waiter(&self, mask: u32, waker: Arc<dyn Waker>);

    /// Снимает `waker` по identity (`Arc::ptr_eq`). Возвращает `true`, если
    /// запись действительно была удалена.
    fn remove_waiter(&self, waker: &Arc<dyn Waker>) -> bool;
}

/// Коллбек снятия начатого ожидания. Взводится на слоте handle-таблицы
/// ([`HandleTable::register_cancel`](crate::HandleTable::register_cancel)) и на текущем потоке
/// ([`KernelRuntime::set_blocked_cancel`](crate::KernelRuntime::set_blocked_cancel)).
pub trait CancelTarget: Send + Sync {
    /// Снимает ожидание и будит поток, который его начал. Вызывается при закрытии либо передаче
    /// хендла, на котором ожидание взведено, и при запросе выхода ожидающего потока. После
    /// вызова у ожидания терминальный исход, и цикл парковки потока заканчивается.
    ///
    /// Одна цель взводится на нескольких слотах, и снятие каждого из них вызывает метод заново,
    /// а реализация терпит повторы. Вызов идёт вне лока handle-таблицы.
    fn cancel(&self);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ParkResolution {
    Signaled,
    Timeout,
    Canceled,
}

pub(super) struct ParkWaker {
    state: AtomicU32,
    observed: AtomicU32,
    winner_index: AtomicU32,
    target: WakeTarget,
}

pub(super) enum WakeTarget {
    Thread {
        runtime: Arc<dyn KernelRuntime>,
        token: WaitToken,
    },
    Object(Arc<super::signal::Signal>),
}

impl WakeTarget {
    fn wake(&self, handoff: bool) {
        match self {
            Self::Thread { runtime, token } if handoff => runtime.unblock_handoff(*token),
            Self::Thread { runtime, token } => runtime.unblock(*token),
            Self::Object(signal) => {
                signal.signal(syscall::SIGNALED, 0);
            }
        }
    }
}

impl ParkWaker {
    pub(super) const NO_WINNER: u32 = u32::MAX;

    pub(super) fn new(target: WakeTarget) -> Self {
        Self {
            state: AtomicU32::new(ParkState::REGISTERED),
            observed: AtomicU32::new(0),
            winner_index: AtomicU32::new(Self::NO_WINNER),
            target,
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
        self.signal_with(index, observed, false);
    }

    fn signal_with(&self, index: u32, observed: u32, handoff: bool) {
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
            self.target.wake(handoff);
        }
    }

    pub(super) fn signal_match(&self) {
        self.signal(0, 0);
    }

    pub(super) fn signal_handoff(&self) {
        self.signal_with(0, 0, true);
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

    pub(super) fn resolve(&self) -> ParkResolution {
        match self.state.load(Ordering::Acquire) {
            ParkState::SIGNALED => ParkResolution::Signaled,
            ParkState::CANCELED => ParkResolution::Canceled,
            ParkState::TIMEOUT => ParkResolution::Timeout,
            _ => {
                if self.claim_timeout() {
                    ParkResolution::Timeout
                } else if self.state.load(Ordering::Acquire) == ParkState::CANCELED {
                    ParkResolution::Canceled
                } else {
                    ParkResolution::Signaled
                }
            }
        }
    }
}

impl CancelTarget for ParkWaker {
    fn cancel(&self) {
        if self.claim_cancel() {
            self.target.wake(false);
        }
    }
}

/// Момент истечения срока `timeout_ns`, отсчитанного от текущего времени `runtime`. `None` -
/// срок без ограничения.
pub fn deadline_after(runtime: &Arc<dyn KernelRuntime>, timeout_ns: Option<u64>) -> Option<u64> {
    timeout_ns.map(|timeout| runtime.now_ns().saturating_add(timeout))
}

pub(super) fn remaining_until(
    runtime: &Arc<dyn KernelRuntime>,
    deadline_ns: Option<u64>,
) -> Option<u64> {
    deadline_ns.map(|deadline| deadline.saturating_sub(runtime.now_ns()))
}

pub(super) fn park_until_deadline(
    runtime: &Arc<dyn KernelRuntime>,
    waker: &Arc<ParkWaker>,
    deadline_ns: Option<u64>,
) {
    while waker.state().load(Ordering::Acquire) == ParkState::REGISTERED {
        let remaining = remaining_until(runtime, deadline_ns);
        if remaining == Some(0) {
            return;
        }
        runtime.block_current_until(waker.state(), remaining);
    }
}

impl Waker for ParkWaker {
    fn wake(&self, observed: u32) {
        self.signal(0, observed);
    }
}

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
            handle_table::HandleTable,
            runtime::{KernelRuntime, WaitToken},
            signal::Signal,
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
        fn now_ns(&self) -> u64 {
            0
        }
        fn current_handle_table(&self) -> Option<Arc<kbase::MutexCell<HandleTable>>> {
            None
        }
        fn exit_current_thread(&self, _exit_code: i32) -> ! {
            unreachable!("not used in wait.rs tests")
        }
        fn exit_current_process(&self, _exit_code: i32) -> ! {
            unreachable!("not used in wait.rs tests")
        }
        fn yield_current(&self) {}
        fn block_current_until(&self, _ready_flag: &AtomicU32, _timeout_ns: Option<u64>) {}
        fn unblock(&self, _token: WaitToken) {
            self.unblocks.fetch_add(1, Ordering::AcqRel);
        }
        fn set_blocked_cancel(&self, _cancel: Arc<dyn CancelTarget>) {}
        fn clear_blocked_cancel(&self) {}
        fn release_process_holder(&self, _process: &Arc<crate::ProcessObject>) {}
    }

    fn make_waker(runtime: Arc<dyn KernelRuntime>) -> Arc<ParkWaker> {
        Arc::new(ParkWaker::new(WakeTarget::Thread {
            runtime,
            token: WaitToken::new(NonZeroU64::new(1).unwrap()),
        }))
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

    #[test]
    fn park_waker_observed_survives_signal_cleared() {
        let s = Signal::new();
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
    fn indexed_waker_observed_and_index_survive_signal_cleared() {
        let s = Signal::new();
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
    fn indexed_waker_resolves_correct_index_across_signals() {
        let s0 = Signal::new();
        let s1 = Signal::new();
        let s2 = Signal::new();

        let rt = StubRuntime::new();
        let waker = make_waker(rt.clone() as Arc<dyn KernelRuntime>);
        let i0: Arc<dyn Waker> = Arc::new(IndexedWaker::new(waker.clone(), 0));
        let i1: Arc<dyn Waker> = Arc::new(IndexedWaker::new(waker.clone(), 1));
        let i2: Arc<dyn Waker> = Arc::new(IndexedWaker::new(waker.clone(), 2));

        s0.register_waiter(BIT0, i0);
        s1.register_waiter(BIT0, i1);
        s2.register_waiter(BIT0, i2);

        s1.signal(BIT0, 0);
        s1.signal(0, BIT0);

        assert_eq!(waker.state().load(Ordering::Acquire), ParkState::SIGNALED);
        assert_eq!(waker.winner_index(), 1);
        assert_eq!(waker.observed() & BIT0, BIT0);
    }

    #[test]
    fn race_signal_and_cancel_unblock_once() {
        use std::{
            sync::{Arc as StdArc, Barrier},
            thread,
        };

        for _ in 0..128 {
            let runtime = StubRuntime::new();
            let waker = make_waker(runtime.clone() as Arc<dyn KernelRuntime>);
            let barrier = StdArc::new(Barrier::new(3));

            let signal_waker = waker.clone();
            let signal_barrier = barrier.clone();
            let signal = thread::spawn(move || {
                signal_barrier.wait();
                Waker::wake(signal_waker.as_ref(), BIT0);
            });

            let cancel_waker = waker.clone();
            let cancel_barrier = barrier.clone();
            let cancel = thread::spawn(move || {
                cancel_barrier.wait();
                CancelTarget::cancel(cancel_waker.as_ref());
            });

            barrier.wait();
            signal.join().expect("signal contender joins");
            cancel.join().expect("cancel contender joins");

            let state = waker.state().load(Ordering::Acquire);
            assert!(state == ParkState::SIGNALED || state == ParkState::CANCELED);
            assert_eq!(runtime.unblock_count(), 1);
        }
    }
}
