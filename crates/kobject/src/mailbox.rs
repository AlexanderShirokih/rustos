//! `Mailbox` KO: many-to-one сборщик пакетов с bounded-очередью.
//!
//! User-код пушит пакеты через [`Mailbox::queue`]. Получатель один:
//! [`Mailbox::try_pop`] атомарно достаёт первый пакет, подъём/сброс
//! [`MAILBOX_READABLE`] сериализован с queue-операциями под одним
//! локом - паттерн, идентичный `Channel`. Wait+pop собирается в
//! `api`-слое поверх [`Mailbox::try_pop`] и общего wait-хелпера.
//!
//! Помимо явных `queue`-вызовов, mailbox умеет подписываться на
//! сигналы других KO: при подписке создаётся per-source
//! `ObserverWaker`, который регистрируется как обычный waker в
//! [`SignalState`] target'а. На срабатывании waker'а ядро складывает в
//! очередь mailbox'а signal-пакет с `(key, observed)` и снимает себя
//! из коллекции observers; backpressure - drop_newest, счётчик
//! [`Mailbox::overflow_count`] инкрементится.

use alloc::{
    collections::VecDeque,
    sync::{Arc, Weak},
    vec::Vec,
};
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use collections::{LockCell, MutexCell};

use super::{
    errors::IpcError,
    koid::Koid,
    wait::{SignalSource, SignalState, Waker},
};

/// Сигнал "в очереди есть хотя бы один пакет".
pub const MAILBOX_READABLE: u32 = 1 << 0;

/// Стартовая ёмкость очереди пакетов. Bounded: при заполнении
/// [`Mailbox::queue`] возвращает [`IpcError::ShouldWait`].
pub const MAILBOX_QUEUE_CAPACITY: usize = 64;

/// Максимальное число активных signal-подписок на один mailbox.
pub const MAILBOX_OBSERVER_CAPACITY: usize = 64;

/// Размер inline-payload в пакете (байт). Часть wire-формата: signal-
/// пакет занимает первые 8 байт под `(trigger, observed)`, оставшиеся
/// 8 - паддинг.
pub const MAILBOX_PAYLOAD_SIZE: usize = 16;

/// Размер сериализованного пакета (байт). Часть syscall ABI;
/// используется при копировании в/из user-памяти.
pub const MAILBOX_PACKET_SIZE: usize = 32;

/// Тип пакета, маршрутизируется получателем.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum MailboxPacketKind {
    /// Поставлен через [`Mailbox::queue`]. `payload` - opaque для ядра.
    User = 0,
    /// Доставка одноразовой signal-подписки.
    SignalOnce = 1,
    /// Доставка repeating signal-подписки.
    SignalRepeating = 2,
}

/// Режим signal-подписки mailbox'а на target.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AsyncMode {
    /// Доставка ровно одного пакета; после доставки waker автоматически
    /// снимается у target'а.
    Once,
    /// Доставка пакета на каждое срабатывание сигнала; observer остаётся
    /// активным до явного [`Mailbox::cancel_subscription`] либо drop'а
    /// mailbox/target.
    Repeating,
}

/// Единица передачи через mailbox.
///
/// Layout фиксирован под syscall ABI: 8 B `key` + 1 B `kind` +
/// 3 B padding + 4 B `status` + 16 B `payload` = 32 B; копируется как
/// blob, поэтому `repr(C)`.
#[derive(Debug, Clone, Copy)]
#[repr(C)]
pub struct MailboxPacket {
    pub key: u64,
    pub kind: MailboxPacketKind,
    pub status: i32,
    pub payload: [u8; MAILBOX_PAYLOAD_SIZE],
}

impl MailboxPacket {
    /// Конструктор user-пакета: `kind = User`, `status = 0`.
    pub const fn user(key: u64, payload: [u8; MAILBOX_PAYLOAD_SIZE]) -> Self {
        Self {
            key,
            kind: MailboxPacketKind::User,
            status: 0,
            payload,
        }
    }

    /// Конструктор signal-пакета: первые 4 байта payload - `mask`
    /// подписки, следующие 4 - `observed` биты в момент wake. `kind`
    /// различает one-shot и repeating-режимы подписки.
    fn signal(kind: MailboxPacketKind, key: u64, mask: u32, observed: u32) -> Self {
        let mut payload = [0u8; MAILBOX_PAYLOAD_SIZE];
        payload[0..4].copy_from_slice(&mask.to_le_bytes());
        payload[4..8].copy_from_slice(&observed.to_le_bytes());
        Self {
            key,
            kind,
            status: 0,
            payload,
        }
    }
}

/// Запись об активной signal-подписке mailbox'а на target.
/// `key` и `target` (Weak) живут в [`ObserverWaker`]; здесь только
/// то, что нужно для поиска подписки в коллекции и общий handle на
/// waker для cancel/drop-путей.
struct Observer {
    target_koid: Koid,
    waker: Arc<ObserverWaker>,
}

/// Per-subscription waker-shim: сидит в waiters-list у target'а,
/// при wake заталкивает signal-пакет в свой mailbox. В Once-режиме
/// после доставки снимает себя из mailbox'овой коллекции observers;
/// в Repeating - re-register'ится в waiters-list target'а и остаётся
/// в коллекции до явного cancel'а или drop'а.
struct ObserverWaker {
    key: u64,
    mask: u32,
    mode: AsyncMode,
    mailbox: Weak<Mailbox>,
    target: Weak<dyn SignalSource>,
    /// `true` означает "подписка более не активна" - либо доставлено
    /// (Once), либо отменено / mailbox дропнут (оба режима). Wake-путь
    /// проверяет флаг на входе и не делает push / re-register.
    inactive: AtomicBool,
}

impl Waker for ObserverWaker {
    fn wake(&self, observed: u32) {
        match self.mode {
            AsyncMode::Once => self.wake_once(observed),
            AsyncMode::Repeating => self.wake_repeating(observed),
        }
    }
}

impl ObserverWaker {
    fn wake_once(&self, observed: u32) {
        if self
            .inactive
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return;
        }
        #[cfg(test)]
        test_hooks::run_wake_once_before_cleanup();
        let Some(mailbox) = self.mailbox.upgrade() else {
            return;
        };
        let packet =
            MailboxPacket::signal(MailboxPacketKind::SignalOnce, self.key, self.mask, observed);
        let self_ptr: *const ObserverWaker = self;
        mailbox.inner.with_lock(|inner| {
            // Снимаем себя из observers под mailbox.inner-локом
            // независимо от того, влез пакет в очередь или дропнулся:
            // подписка одноразовая, source.signals() уже снял waker'а
            // из своего waiters-list при срабатывании.
            inner
                .observers
                .retain(|o| !core::ptr::eq(Arc::as_ptr(&o.waker), self_ptr));
            if inner.queue.len() >= inner.capacity {
                mailbox.overflow_count.fetch_add(1, Ordering::AcqRel);
                return;
            }
            inner.queue.push_back(packet);
            mailbox.signals.signal(MAILBOX_READABLE, 0);
        });
    }

    fn wake_repeating(&self, observed: u32) {
        // Раннее отсечение для cancelled/dropped подписки: cancel и
        // Drop у Mailbox ставят флаг до похода в target.waiters, а
        // signal() уже мог вытолкнуть нас оттуда раньше - значит
        // повторного wake уже не будет, и работа здесь не нужна.
        if self.inactive.load(Ordering::Acquire) {
            return;
        }
        let Some(mailbox) = self.mailbox.upgrade() else {
            return;
        };
        let packet = MailboxPacket::signal(
            MailboxPacketKind::SignalRepeating,
            self.key,
            self.mask,
            observed,
        );
        let self_ptr: *const ObserverWaker = self;

        // Под mailbox.inner: убедиться, что cancel не успел снять
        // observer'а; положить пакет (или зафиксировать overflow);
        // получить собственный Arc<ObserverWaker> для re-register'а.
        // Re-register выносим за лок: target.waiters берётся раньше
        // mailbox.inner в lock-order'е (см. subscribe).
        let waker_self: Option<Arc<ObserverWaker>> = mailbox.inner.with_lock(|inner| {
            let waker_self = inner
                .observers
                .iter()
                .find(|o| core::ptr::eq(Arc::as_ptr(&o.waker), self_ptr))
                .map(|o| o.waker.clone())?;
            if inner.queue.len() >= inner.capacity {
                mailbox.overflow_count.fetch_add(1, Ordering::AcqRel);
            } else {
                inner.queue.push_back(packet);
                mailbox.signals.signal(MAILBOX_READABLE, 0);
            }
            Some(waker_self)
        });

        let Some(waker_self) = waker_self else {
            return;
        };
        let Some(target) = self.target.upgrade() else {
            return;
        };
        let waker_dyn: Arc<dyn Waker> = waker_self;
        // Silent re-register: fast-path wake на ещё не сброшенном
        // уровне сигналов превратил бы доставку в рекурсию. Следующий
        // вызов signal() сам решит, проснуть нас или нет.
        target
            .signals()
            .register_waiter_silent(self.mask, waker_dyn.clone());

        // Cancel/drop мог сработать после нашей публикации waker'а в
        // target.waiters: post-check + remove защищает от утечки
        // подписки в waiters-list уже снятого observer'а.
        if self.inactive.load(Ordering::Acquire) {
            target.signals().remove_waiter(&waker_dyn);
        }
    }
}

struct MailboxInner {
    queue: VecDeque<MailboxPacket>,
    capacity: usize,
    observers: Vec<Observer>,
}

impl MailboxInner {
    fn with_capacity(capacity: usize) -> Self {
        Self {
            // Префиксная аллокация - push_back в hot path не делает realloc.
            queue: VecDeque::with_capacity(capacity),
            capacity,
            observers: Vec::with_capacity(MAILBOX_OBSERVER_CAPACITY),
        }
    }
}

/// Many-to-one сборщик пакетов. Сигналим [`MAILBOX_READABLE`] при
/// непустой очереди; сигнал и pop/push атомарны под одним локом.
pub struct Mailbox {
    inner: MutexCell<MailboxInner>,
    signals: SignalState,
    overflow_count: AtomicU64,
}

impl Mailbox {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            inner: MutexCell::new(MailboxInner::with_capacity(MAILBOX_QUEUE_CAPACITY)),
            signals: SignalState::new(0),
            overflow_count: AtomicU64::new(0),
        })
    }

    /// Прямой доступ к [`SignalState`] для интеграции с wait-путём.
    pub fn signals(&self) -> &SignalState {
        &self.signals
    }

    /// Снимок сигналов без блокировок.
    pub fn peek_signals(&self) -> u32 {
        self.signals.peek()
    }

    /// Счётчик дропнутых signal-пакетов. Растёт монотонно при
    /// доставке в полную очередь.
    pub fn overflow_count(&self) -> u64 {
        self.overflow_count.load(Ordering::Acquire)
    }

    /// Помещает пакет в очередь. На полной очереди - [`IpcError::ShouldWait`]
    /// без побочных эффектов. На успехе атомарно поднимает
    /// [`MAILBOX_READABLE`].
    pub fn queue(&self, packet: MailboxPacket) -> Result<(), IpcError> {
        self.inner.with_lock(|inner| {
            if inner.queue.len() >= inner.capacity {
                return Err(IpcError::ShouldWait);
            }
            inner.queue.push_back(packet);
            // signal под тем же локом, что и push: при опустошении
            // в `try_pop` под этим же локом снимаем бит - окна, в
            // которое readable-bit разойдётся с состоянием очереди,
            // не остаётся.
            self.signals.signal(MAILBOX_READABLE, 0);
            Ok(())
        })
    }

    /// Достаёт первый пакет. На пустой очереди - [`IpcError::ShouldWait`].
    /// При опустошении атомарно снимает [`MAILBOX_READABLE`].
    pub fn try_pop(&self) -> Result<MailboxPacket, IpcError> {
        self.inner.with_lock(|inner| {
            let packet = inner.queue.pop_front().ok_or(IpcError::ShouldWait)?;
            if inner.queue.is_empty() {
                self.signals.signal(0, MAILBOX_READABLE);
            }
            Ok(packet)
        })
    }

    /// Регистрирует observer-подписку: при поднятии хотя бы одного
    /// бита из `mask` у `target` в очередь mailbox'а будет помещён
    /// signal-пакет с `key` и текущим `observed`. В Once-режиме
    /// доставляется ровно один пакет; в Repeating - по одному на
    /// каждое срабатывание сигнала, до явного [`cancel_subscription`]
    /// либо drop'а mailbox/target.
    ///
    /// Lock order: target.signals.waiters -> mailbox.inner.
    pub(super) fn subscribe(
        self: &Arc<Self>,
        target: &Arc<dyn SignalSource>,
        target_koid: Koid,
        key: u64,
        mask: u32,
        mode: AsyncMode,
    ) -> Result<(), IpcError> {
        self.inner.with_lock(|inner| {
            inner
                .observers
                .retain(|o| !o.waker.inactive.load(Ordering::Acquire));
            if inner
                .observers
                .iter()
                .any(|o| o.target_koid == target_koid && o.waker.key == key)
            {
                return Ok(());
            }
            if inner.observers.len() >= MAILBOX_OBSERVER_CAPACITY {
                return Err(IpcError::OutOfHandles);
            }
            Ok(())
        })?;

        let waker = Arc::new(ObserverWaker {
            key,
            mask,
            mode,
            mailbox: Arc::downgrade(self),
            target: Arc::downgrade(target),
            inactive: AtomicBool::new(false),
        });

        let inserted = self.inner.with_lock(|inner| {
            inner
                .observers
                .retain(|o| !o.waker.inactive.load(Ordering::Acquire));
            if inner
                .observers
                .iter()
                .any(|o| o.target_koid == target_koid && o.waker.key == key)
            {
                return Ok(false);
            }
            if inner.observers.len() >= MAILBOX_OBSERVER_CAPACITY {
                return Err(IpcError::OutOfHandles);
            }
            inner.observers.push(Observer {
                target_koid,
                waker: waker.clone(),
            });
            Ok(true)
        })?;

        if inserted {
            let waker_dyn: Arc<dyn Waker> = waker;
            target.signals().register_waiter(mask, waker_dyn);
        }

        Ok(())
    }

    /// Снимает подписку по `(target_koid, key)`. Идемпотентна: если
    /// подписки нет либо target уже дропнут - `Ok(())`.
    ///
    /// Lock order: mailbox.inner -> (отпустить) -> target.waiters.
    /// Брать оба лока одновременно нельзя (см. `subscribe`).
    pub(super) fn cancel_subscription(&self, target_koid: Koid, key: u64) {
        let waker = self.inner.with_lock(|inner| {
            let pos = inner
                .observers
                .iter()
                .position(|o| o.target_koid == target_koid && o.waker.key == key)?;
            Some(inner.observers.swap_remove(pos).waker)
        });

        let Some(waker) = waker else {
            return;
        };

        // Закрываем гонку с поздним wake: даже если SignalState уже
        // вызвал ObserverWaker::wake между нашим swap_remove и текущей
        // строкой, проверка `inactive` гарантирует, что второй вход в
        // wake (из нашей стороны) не произойдёт; первый, если успел,
        // проскочит self-removal-цикл уже без observer'а в коллекции.
        waker.inactive.store(true, Ordering::Release);

        // Если target ещё жив - снимаем waker'а из его waiters-list.
        // Если уже дропнут - waker и так unreachable, утечки нет.
        if let Some(target) = waker.target.upgrade() {
            let waker_dyn: Arc<dyn Waker> = waker;
            target.signals().remove_waiter(&waker_dyn);
        }
    }
}

impl SignalSource for Mailbox {
    fn signals(&self) -> &SignalState {
        &self.signals
    }
}

impl Drop for Mailbox {
    fn drop(&mut self) {
        // Снимаем все ещё активные observer-подписки: ставим
        // inactive=true, чтобы поздний wake был no-op, и снимаем
        // waker'а из waiters-list у target'а, если тот ещё жив.
        let observers = self.inner.with_lock(|inner| {
            inner.queue.clear();
            core::mem::take(&mut inner.observers)
        });
        for obs in observers {
            obs.waker.inactive.store(true, Ordering::Release);
            if let Some(target) = obs.waker.target.upgrade() {
                let waker_dyn: Arc<dyn Waker> = obs.waker;
                target.signals().remove_waiter(&waker_dyn);
            }
        }
    }
}

#[cfg(test)]
mod test_hooks {
    use alloc::sync::Arc;
    use std::sync::{Mutex, MutexGuard, OnceLock};

    type Hook = Arc<dyn Fn() + Send + Sync>;

    fn test_lock() -> &'static Mutex<()> {
        static TEST_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        TEST_LOCK.get_or_init(|| Mutex::new(()))
    }

    fn wake_once_hook() -> &'static Mutex<Option<Hook>> {
        static WAKE_ONCE_HOOK: OnceLock<Mutex<Option<Hook>>> = OnceLock::new();
        WAKE_ONCE_HOOK.get_or_init(|| Mutex::new(None))
    }

    pub(super) struct HookGuard {
        _lock: MutexGuard<'static, ()>,
    }

    impl Drop for HookGuard {
        fn drop(&mut self) {
            *wake_once_hook().lock().unwrap() = None;
        }
    }

    pub(super) fn install_wake_once_before_cleanup(hook: Hook) -> HookGuard {
        let lock = test_lock().lock().unwrap();
        *wake_once_hook().lock().unwrap() = Some(hook);
        HookGuard { _lock: lock }
    }

    pub(super) fn run_wake_once_before_cleanup() {
        let hook = wake_once_hook().lock().unwrap().clone();
        if let Some(hook) = hook {
            hook();
        }
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use super::{
        super::{
            channel::{CHANNEL_PEER_CLOSED, CHANNEL_READABLE, Channel, Message},
            event::{EVENT_SIGNALED, Event},
            object::KObject,
            wait::MockWaker,
        },
        *,
    };

    fn upayload(b: &[u8]) -> [u8; MAILBOX_PAYLOAD_SIZE] {
        let mut buf = [0u8; MAILBOX_PAYLOAD_SIZE];
        buf[..b.len()].copy_from_slice(b);
        buf
    }

    fn payload(b: &[u8]) -> Message {
        Message::from_bytes(b).expect("payload fits")
    }

    #[test]
    fn queue_then_pop_round_trip() {
        let mb = Mailbox::new();
        mb.queue(MailboxPacket::user(0x42, upayload(b"hi")))
            .unwrap();
        let got = mb.try_pop().unwrap();
        assert_eq!(got.key, 0x42);
        assert_eq!(got.kind, MailboxPacketKind::User);
        assert_eq!(&got.payload[..2], b"hi");
    }

    #[test]
    fn pop_empty_returns_should_wait() {
        let mb = Mailbox::new();
        assert_eq!(mb.try_pop().unwrap_err(), IpcError::ShouldWait);
    }

    #[test]
    fn queue_into_full_returns_should_wait() {
        let mb = Mailbox::new();
        for i in 0..MAILBOX_QUEUE_CAPACITY as u64 {
            mb.queue(MailboxPacket::user(i, [0; MAILBOX_PAYLOAD_SIZE]))
                .unwrap();
        }
        assert_eq!(
            mb.queue(MailboxPacket::user(99, [0; MAILBOX_PAYLOAD_SIZE]))
                .unwrap_err(),
            IpcError::ShouldWait
        );

        // После одного pop'а место освобождается.
        mb.try_pop().unwrap();
        mb.queue(MailboxPacket::user(99, [0; MAILBOX_PAYLOAD_SIZE]))
            .unwrap();
    }

    #[test]
    fn readable_set_on_first_queue_cleared_on_drain() {
        let mb = Mailbox::new();
        assert_eq!(mb.peek_signals() & MAILBOX_READABLE, 0);

        mb.queue(MailboxPacket::user(1, [0; MAILBOX_PAYLOAD_SIZE]))
            .unwrap();
        assert_eq!(mb.peek_signals() & MAILBOX_READABLE, MAILBOX_READABLE);

        mb.try_pop().unwrap();
        assert_eq!(mb.peek_signals() & MAILBOX_READABLE, 0);
    }

    #[test]
    fn pop_keeps_readable_when_queue_non_empty() {
        let mb = Mailbox::new();
        for i in 0..3u64 {
            mb.queue(MailboxPacket::user(i, [0; MAILBOX_PAYLOAD_SIZE]))
                .unwrap();
        }
        assert_eq!(mb.peek_signals() & MAILBOX_READABLE, MAILBOX_READABLE);

        mb.try_pop().unwrap();
        assert_eq!(mb.peek_signals() & MAILBOX_READABLE, MAILBOX_READABLE);
        mb.try_pop().unwrap();
        assert_eq!(mb.peek_signals() & MAILBOX_READABLE, MAILBOX_READABLE);
        mb.try_pop().unwrap();
        assert_eq!(mb.peek_signals() & MAILBOX_READABLE, 0);
    }

    #[test]
    fn waiter_woken_on_first_queue() {
        let mb = Mailbox::new();
        let w = MockWaker::new();
        mb.signals().register_waiter(MAILBOX_READABLE, w.clone());
        assert!(!w.was_woken());

        mb.queue(MailboxPacket::user(7, [0; MAILBOX_PAYLOAD_SIZE]))
            .unwrap();
        assert!(w.was_woken());
        assert_eq!(w.observed() & MAILBOX_READABLE, MAILBOX_READABLE);
    }

    #[test]
    fn drop_mailbox_with_pending_does_not_panic() {
        let mb = Mailbox::new();
        for i in 0..5u64 {
            mb.queue(MailboxPacket::user(i, [0; MAILBOX_PAYLOAD_SIZE]))
                .unwrap();
        }
        drop(mb);
    }

    #[test]
    fn fifo_order_preserved() {
        let mb = Mailbox::new();
        for i in 0..10u64 {
            mb.queue(MailboxPacket::user(i, [0; MAILBOX_PAYLOAD_SIZE]))
                .unwrap();
        }
        for i in 0..10u64 {
            assert_eq!(mb.try_pop().unwrap().key, i);
        }
    }

    #[test]
    fn overflow_count_starts_at_zero() {
        let mb = Mailbox::new();
        assert_eq!(mb.overflow_count(), 0);
    }

    #[test]
    fn concurrent_queue_and_pop_keeps_readable_consistent() {
        use std::{sync::Barrier, thread};

        const ITERATIONS: u32 = 50_000;

        for iteration in 0..ITERATIONS {
            let mb = Mailbox::new();
            mb.queue(MailboxPacket::user(1, [0; MAILBOX_PAYLOAD_SIZE]))
                .unwrap();

            let barrier = Arc::new(Barrier::new(2));
            let mb_w = mb.clone();
            let barrier_w = barrier.clone();
            let writer = thread::spawn(move || {
                barrier_w.wait();
                mb_w.queue(MailboxPacket::user(2, [0; MAILBOX_PAYLOAD_SIZE]))
                    .unwrap();
            });

            barrier.wait();
            let first = mb.try_pop().expect("first pop succeeds");
            writer.join().unwrap();

            assert_eq!(first.key, 1);
            assert_eq!(
                mb.peek_signals() & MAILBOX_READABLE,
                MAILBOX_READABLE,
                "iteration {iteration}: READABLE потерян после race queue/pop"
            );

            assert_eq!(mb.try_pop().unwrap().key, 2);
            assert_eq!(mb.peek_signals() & MAILBOX_READABLE, 0);
        }
    }

    fn target_event(ev: &Arc<Event>) -> (Arc<dyn SignalSource>, Koid) {
        let dyn_arc: Arc<dyn SignalSource> = ev.clone();
        (dyn_arc, KObject::Event(ev.clone()).koid())
    }

    fn target_channel(ch: &Arc<Channel>) -> (Arc<dyn SignalSource>, Koid) {
        let dyn_arc: Arc<dyn SignalSource> = ch.clone();
        (dyn_arc, KObject::Channel(ch.clone()).koid())
    }

    fn target_mailbox(mb: &Arc<Mailbox>) -> (Arc<dyn SignalSource>, Koid) {
        let dyn_arc: Arc<dyn SignalSource> = mb.clone();
        (dyn_arc, KObject::Mailbox(mb.clone()).koid())
    }

    #[test]
    fn wait_async_once_delivers_packet_on_event_signal() {
        let mb = Mailbox::new();
        let ev = Event::new();
        let (target, koid) = target_event(&ev);
        mb.subscribe(&target, koid, 0xDEAD_BEEF, EVENT_SIGNALED, AsyncMode::Once)
            .unwrap();

        assert!(mb.try_pop().is_err());
        ev.signal(EVENT_SIGNALED, 0);

        let pkt = mb.try_pop().expect("packet delivered");
        assert_eq!(pkt.key, 0xDEAD_BEEF);
        assert_eq!(pkt.kind, MailboxPacketKind::SignalOnce);
        let mask = u32::from_le_bytes(pkt.payload[0..4].try_into().unwrap());
        let observed = u32::from_le_bytes(pkt.payload[4..8].try_into().unwrap());
        assert_eq!(mask, EVENT_SIGNALED);
        assert_eq!(observed & EVENT_SIGNALED, EVENT_SIGNALED);
    }

    #[test]
    fn wait_async_once_does_not_redeliver() {
        let mb = Mailbox::new();
        let ev = Event::new();
        let (target, koid) = target_event(&ev);
        mb.subscribe(&target, koid, 1, EVENT_SIGNALED, AsyncMode::Once)
            .unwrap();

        ev.signal(EVENT_SIGNALED, 0);
        mb.try_pop().expect("first delivery");

        // Сбросим и поднимем сигнал ещё раз - повторных пакетов не
        // должно быть, так как Once-waker уже снят из waiters-list у
        // event'а после wake (SignalState.entries.retain).
        ev.signal(0, EVENT_SIGNALED);
        ev.signal(EVENT_SIGNALED, 0);
        assert_eq!(mb.try_pop().unwrap_err(), IpcError::ShouldWait);
    }

    #[test]
    fn wait_async_on_channel_readable() {
        let mb = Mailbox::new();
        let (a, b) = Channel::create_pair(4);
        let (target, koid) = target_channel(&b);
        mb.subscribe(&target, koid, 7, CHANNEL_READABLE, AsyncMode::Once)
            .unwrap();

        a.write(payload(b"hi")).unwrap();
        let pkt = mb.try_pop().expect("packet delivered");
        assert_eq!(pkt.key, 7);
        assert_eq!(pkt.kind, MailboxPacketKind::SignalOnce);
    }

    #[test]
    fn wait_async_on_channel_peer_closed() {
        let mb = Mailbox::new();
        let (a, b) = Channel::create_pair(4);
        let (target, koid) = target_channel(&b);
        mb.subscribe(&target, koid, 9, CHANNEL_PEER_CLOSED, AsyncMode::Once)
            .unwrap();

        drop(a);
        let pkt = mb.try_pop().expect("packet delivered");
        assert_eq!(pkt.key, 9);
    }

    #[test]
    fn cancel_removes_observer_before_signal() {
        let mb = Mailbox::new();
        let ev = Event::new();
        let (target, koid) = target_event(&ev);
        mb.subscribe(&target, koid, 1, EVENT_SIGNALED, AsyncMode::Once)
            .unwrap();
        mb.cancel_subscription(koid, 1);

        ev.signal(EVENT_SIGNALED, 0);
        assert_eq!(mb.try_pop().unwrap_err(), IpcError::ShouldWait);
    }

    #[test]
    fn cancel_idempotent_when_no_subscription() {
        let mb = Mailbox::new();
        let ev = Event::new();
        let (_target, koid) = target_event(&ev);
        // Нет активной подписки - cancel не должен паниковать.
        mb.cancel_subscription(koid, 42);
    }

    #[test]
    fn cancel_after_target_dropped_is_ok() {
        let mb = Mailbox::new();
        let ev = Event::new();
        let (target, koid) = target_event(&ev);
        mb.subscribe(&target, koid, 1, EVENT_SIGNALED, AsyncMode::Once)
            .unwrap();
        drop(target);
        drop(ev);

        // Target дропнут - Weak в Observer уже None, cancel должен
        // молча пройти без panic'а: waker и так unreachable, removal
        // из waiters-list не нужен.
        mb.cancel_subscription(koid, 1);
    }

    #[test]
    fn drop_target_before_signal_observer_silent() {
        let mb = Mailbox::new();
        let ev = Event::new();
        let (target, koid) = target_event(&ev);
        mb.subscribe(&target, koid, 1, EVENT_SIGNALED, AsyncMode::Once)
            .unwrap();
        drop(target);
        drop(ev);

        // Никакого пакета не должно быть.
        assert_eq!(mb.try_pop().unwrap_err(), IpcError::ShouldWait);
    }

    #[test]
    fn multiple_sources_fan_in() {
        let mb = Mailbox::new();
        let ev1 = Event::new();
        let ev2 = Event::new();
        let ev3 = Event::new();
        let (t1, k1) = target_event(&ev1);
        let (t2, k2) = target_event(&ev2);
        let (t3, k3) = target_event(&ev3);
        mb.subscribe(&t1, k1, 100, EVENT_SIGNALED, AsyncMode::Once)
            .unwrap();
        mb.subscribe(&t2, k2, 200, EVENT_SIGNALED, AsyncMode::Once)
            .unwrap();
        mb.subscribe(&t3, k3, 300, EVENT_SIGNALED, AsyncMode::Once)
            .unwrap();

        ev2.signal(EVENT_SIGNALED, 0);
        ev1.signal(EVENT_SIGNALED, 0);
        ev3.signal(EVENT_SIGNALED, 0);

        let p1 = mb.try_pop().unwrap();
        let p2 = mb.try_pop().unwrap();
        let p3 = mb.try_pop().unwrap();
        assert_eq!(p1.key, 200);
        assert_eq!(p2.key, 100);
        assert_eq!(p3.key, 300);
    }

    #[test]
    fn duplicate_subscription_is_idempotent() {
        let mb = Mailbox::new();
        let ev = Event::new();
        let (target, koid) = target_event(&ev);

        mb.subscribe(&target, koid, 7, EVENT_SIGNALED, AsyncMode::Once)
            .unwrap();
        mb.subscribe(&target, koid, 7, EVENT_SIGNALED, AsyncMode::Once)
            .unwrap();

        let observer_len = mb.inner.with_lock(|inner| inner.observers.len());
        assert_eq!(observer_len, 1);

        ev.signal(EVENT_SIGNALED, 0);
        let packet = mb.try_pop().unwrap();
        assert_eq!(packet.key, 7);
        assert_eq!(packet.kind, MailboxPacketKind::SignalOnce);
        assert_eq!(mb.try_pop().unwrap_err(), IpcError::ShouldWait);
    }

    #[test]
    fn resubscribe_during_once_delivery_gets_its_own_packet() {
        use std::{
            sync::atomic::{AtomicBool, Ordering as AtomicOrdering},
            sync::{Arc, Barrier},
            thread,
        };

        let mb = Mailbox::new();
        let ev = Event::new();
        let (target, koid) = target_event(&ev);
        let entered = Arc::new(Barrier::new(2));
        let release = Arc::new(Barrier::new(2));
        let first_delivery = Arc::new(AtomicBool::new(true));
        let entered_hook = entered.clone();
        let release_hook = release.clone();
        let first_delivery_hook = first_delivery.clone();
        let _hook_guard = super::test_hooks::install_wake_once_before_cleanup(Arc::new(
            move || {
                if first_delivery_hook.swap(false, AtomicOrdering::AcqRel) {
                    entered_hook.wait();
                    release_hook.wait();
                }
            },
        ));

        mb.subscribe(&target, koid, 7, EVENT_SIGNALED, AsyncMode::Once)
            .unwrap();

        let ev_for_signal = ev.clone();
        let signaller = thread::spawn(move || {
            ev_for_signal.signal(EVENT_SIGNALED, 0);
        });

        entered.wait();

        mb.subscribe(&target, koid, 7, EVENT_SIGNALED, AsyncMode::Once)
            .unwrap();

        release.wait();
        signaller.join().unwrap();

        let packet_a = mb.try_pop().unwrap();
        let packet_b = mb.try_pop().unwrap();
        let packets = [(packet_a.key, packet_a.kind), (packet_b.key, packet_b.kind)];
        assert!(packets.contains(&(7, MailboxPacketKind::SignalOnce)));
        assert!(packets.iter().all(|&(key, kind)| key == 7 && kind == MailboxPacketKind::SignalOnce));
        assert_eq!(mb.try_pop().unwrap_err(), IpcError::ShouldWait);
    }

    #[test]
    fn subscription_limit_returns_out_of_handles() {
        let mb = Mailbox::new();
        let ev = Event::new();
        let (target, koid) = target_event(&ev);

        for key in 0..MAILBOX_OBSERVER_CAPACITY as u64 {
            mb.subscribe(&target, koid, key, EVENT_SIGNALED, AsyncMode::Once)
                .unwrap();
        }

        assert_eq!(
            mb.subscribe(
                &target,
                koid,
                MAILBOX_OBSERVER_CAPACITY as u64,
                EVENT_SIGNALED,
                AsyncMode::Once,
            )
            .unwrap_err(),
            IpcError::OutOfHandles
        );
        let observer_len = mb.inner.with_lock(|inner| inner.observers.len());
        assert_eq!(observer_len, MAILBOX_OBSERVER_CAPACITY);
    }

    #[test]
    fn overflow_drops_signal_packet_and_increments_counter() {
        let mb = Mailbox::new();
        for i in 0..MAILBOX_QUEUE_CAPACITY as u64 {
            mb.queue(MailboxPacket::user(i, [0; MAILBOX_PAYLOAD_SIZE]))
                .unwrap();
        }
        assert_eq!(mb.overflow_count(), 0);

        let ev = Event::new();
        let (target, koid) = target_event(&ev);
        mb.subscribe(&target, koid, 555, EVENT_SIGNALED, AsyncMode::Once)
            .unwrap();
        ev.signal(EVENT_SIGNALED, 0);

        // Очередь не выросла, overflow счётчик +1.
        assert_eq!(mb.overflow_count(), 1);
        let queue_len = mb.inner.with_lock(|inner| inner.queue.len());
        assert_eq!(queue_len, MAILBOX_QUEUE_CAPACITY);
    }

    #[test]
    fn cancel_vs_deliver_race() {
        use std::{sync::Barrier, thread};

        const ITERATIONS: u32 = 50_000;

        for iteration in 0..ITERATIONS {
            let mb = Mailbox::new();
            let ev = Event::new();
            let (target, koid) = target_event(&ev);
            mb.subscribe(&target, koid, 1, EVENT_SIGNALED, AsyncMode::Once)
                .unwrap();

            let barrier = Arc::new(Barrier::new(2));

            let mb_c = mb.clone();
            let barrier_c = barrier.clone();
            let canceller = thread::spawn(move || {
                barrier_c.wait();
                mb_c.cancel_subscription(koid, 1);
            });

            let ev_s = ev.clone();
            let barrier_s = barrier.clone();
            let signaller = thread::spawn(move || {
                barrier_s.wait();
                ev_s.signal(EVENT_SIGNALED, 0);
            });

            canceller.join().unwrap();
            signaller.join().unwrap();

            // Допустимо ровно одно из двух: пакет в очереди (cancel
            // опоздал) или очередь пуста (cancel успел). Никаких
            // дублирований.
            let queue_len = mb.inner.with_lock(|inner| inner.queue.len());
            assert!(
                queue_len <= 1,
                "iteration {iteration}: лишний пакет в очереди ({queue_len})"
            );
        }
    }

    #[test]
    fn signalable_kos_implement_signal_source() {
        // Несигнализуемые Memory/PhysicalResource намеренно не реализуют
        // SignalSource: подписка на них отказывается на api-слое
        // (`WrongType`); здесь - compile-time проверка, что
        // сигнализуемые KO trait действительно имплементируют.
        fn assert_signal_source<T: SignalSource + ?Sized>() {}
        assert_signal_source::<Mailbox>();
        assert_signal_source::<Channel>();
        assert_signal_source::<Event>();
    }

    #[test]
    fn mailbox_subscribed_to_self_handles_self_signal() {
        let mb = Mailbox::new();
        // Push user-пакет -> MAILBOX_READABLE поднят.
        mb.queue(MailboxPacket::user(1, [0; MAILBOX_PAYLOAD_SIZE]))
            .unwrap();
        assert_eq!(mb.peek_signals() & MAILBOX_READABLE, MAILBOX_READABLE);

        // Подписка: register_waiter увидит current=READABLE, сразу
        // вызовет wake -> wake возьмёт mailbox.inner и положит signal-
        // пакет (тот же лок, что внешний subscribe отпускает на
        // момент register_waiter).
        let (target, koid) = target_mailbox(&mb);
        mb.subscribe(&target, koid, 42, MAILBOX_READABLE, AsyncMode::Once)
            .unwrap();

        // В очереди: user(1), signal(42).
        let p1 = mb.try_pop().unwrap();
        assert_eq!(p1.key, 1);
        assert_eq!(p1.kind, MailboxPacketKind::User);

        let p2 = mb.try_pop().unwrap();
        assert_eq!(p2.key, 42);
        assert_eq!(p2.kind, MailboxPacketKind::SignalOnce);

        assert_eq!(mb.try_pop().unwrap_err(), IpcError::ShouldWait);
    }

    #[test]
    fn drop_mailbox_with_active_subscriptions_does_not_panic() {
        let ev = Event::new();
        {
            let mb = Mailbox::new();
            let (target, koid) = target_event(&ev);
            mb.subscribe(&target, koid, 1, EVENT_SIGNALED, AsyncMode::Once)
                .unwrap();
            // mb уходит в drop здесь.
        }
        ev.signal(EVENT_SIGNALED, 0);
    }

    #[test]
    fn wait_async_repeating_delivers_on_each_signal() {
        let mb = Mailbox::new();
        let ev = Event::new();
        let (target, koid) = target_event(&ev);
        mb.subscribe(&target, koid, 7, EVENT_SIGNALED, AsyncMode::Repeating)
            .unwrap();

        ev.signal(EVENT_SIGNALED, EVENT_SIGNALED);
        ev.signal(EVENT_SIGNALED, EVENT_SIGNALED);
        ev.signal(EVENT_SIGNALED, EVENT_SIGNALED);

        for _ in 0..3 {
            let pkt = mb.try_pop().expect("repeating packet");
            assert_eq!(pkt.key, 7);
            assert_eq!(pkt.kind, MailboxPacketKind::SignalRepeating);
        }
        assert_eq!(mb.try_pop().unwrap_err(), IpcError::ShouldWait);
    }

    #[test]
    fn wait_async_repeating_delivers_on_each_channel_write() {
        let mb = Mailbox::new();
        let (a, b) = Channel::create_pair(8);
        let (target, koid) = target_channel(&b);
        mb.subscribe(&target, koid, 11, CHANNEL_READABLE, AsyncMode::Repeating)
            .unwrap();

        for i in 0u8..3 {
            a.write(payload(&[i])).unwrap();
            let msg = b.read().expect("drain channel");
            assert_eq!(msg.bytes(), &[i]);
        }

        for _ in 0..3 {
            let pkt = mb.try_pop().expect("repeating packet");
            assert_eq!(pkt.key, 11);
            assert_eq!(pkt.kind, MailboxPacketKind::SignalRepeating);
        }
        assert!(mb.try_pop().is_err());
    }

    #[test]
    fn wait_async_repeating_re_registers_after_wake() {
        let mb = Mailbox::new();
        let ev = Event::new();
        let (target, koid) = target_event(&ev);
        mb.subscribe(&target, koid, 5, EVENT_SIGNALED, AsyncMode::Repeating)
            .unwrap();

        ev.signal(EVENT_SIGNALED, EVENT_SIGNALED);
        let p1 = mb.try_pop().expect("first packet");
        assert_eq!(p1.kind, MailboxPacketKind::SignalRepeating);

        ev.signal(EVENT_SIGNALED, EVENT_SIGNALED);
        let p2 = mb.try_pop().expect("second packet");
        assert_eq!(p2.kind, MailboxPacketKind::SignalRepeating);
        assert_eq!(p2.key, 5);
    }

    #[test]
    fn cancel_repeating_subscription_stops_delivery() {
        let mb = Mailbox::new();
        let ev = Event::new();
        let (target, koid) = target_event(&ev);
        mb.subscribe(&target, koid, 3, EVENT_SIGNALED, AsyncMode::Repeating)
            .unwrap();

        ev.signal(EVENT_SIGNALED, EVENT_SIGNALED);
        mb.try_pop().expect("first delivery");

        mb.cancel_subscription(koid, 3);

        ev.signal(EVENT_SIGNALED, EVENT_SIGNALED);
        assert_eq!(mb.try_pop().unwrap_err(), IpcError::ShouldWait);
    }

    #[test]
    fn drop_target_stops_repeating_delivery() {
        let mb = Mailbox::new();
        let ev = Event::new();
        let (target, koid) = target_event(&ev);
        mb.subscribe(&target, koid, 4, EVENT_SIGNALED, AsyncMode::Repeating)
            .unwrap();

        ev.signal(EVENT_SIGNALED, EVENT_SIGNALED);
        let p1 = mb.try_pop().expect("first packet");
        assert_eq!(p1.kind, MailboxPacketKind::SignalRepeating);

        drop(target);
        drop(ev);

        // Mailbox валиден: явные queue-операции работают как раньше.
        mb.queue(MailboxPacket::user(99, [0; MAILBOX_PAYLOAD_SIZE]))
            .unwrap();
        let p = mb.try_pop().unwrap();
        assert_eq!(p.kind, MailboxPacketKind::User);
        assert!(mb.try_pop().is_err());
    }

    #[test]
    fn repeating_overflow_increments_counter_keeps_subscription() {
        let mb = Mailbox::new();
        for i in 0..MAILBOX_QUEUE_CAPACITY as u64 {
            mb.queue(MailboxPacket::user(i, [0; MAILBOX_PAYLOAD_SIZE]))
                .unwrap();
        }

        let ev = Event::new();
        let (target, koid) = target_event(&ev);
        mb.subscribe(&target, koid, 777, EVENT_SIGNALED, AsyncMode::Repeating)
            .unwrap();

        ev.signal(EVENT_SIGNALED, EVENT_SIGNALED);
        assert_eq!(mb.overflow_count(), 1);
        let queue_len = mb.inner.with_lock(|inner| inner.queue.len());
        assert_eq!(queue_len, MAILBOX_QUEUE_CAPACITY);

        // Освобождаем место - подписка ещё активна, повторный signal
        // должен принести signal-пакет.
        let popped = mb.try_pop().unwrap();
        assert_eq!(popped.kind, MailboxPacketKind::User);

        ev.signal(EVENT_SIGNALED, EVENT_SIGNALED);

        // В очереди после: оставшиеся user-пакеты + 1 signal на хвосте.
        let queue_len_after = mb.inner.with_lock(|inner| inner.queue.len());
        assert_eq!(queue_len_after, MAILBOX_QUEUE_CAPACITY);
        assert_eq!(mb.overflow_count(), 1);

        // Вычитываем оставшиеся user-пакеты и убеждаемся, что
        // последним пришёл signal-пакет.
        for _ in 0..(MAILBOX_QUEUE_CAPACITY - 1) {
            let p = mb.try_pop().unwrap();
            assert_eq!(p.kind, MailboxPacketKind::User);
        }
        let last = mb.try_pop().unwrap();
        assert_eq!(last.kind, MailboxPacketKind::SignalRepeating);
        assert_eq!(last.key, 777);
    }

    #[test]
    fn mixed_modes_coexist() {
        let mb = Mailbox::new();
        let ev = Event::new();
        let (target, koid) = target_event(&ev);
        mb.subscribe(&target, koid, 100, EVENT_SIGNALED, AsyncMode::Once)
            .unwrap();
        mb.subscribe(&target, koid, 200, EVENT_SIGNALED, AsyncMode::Repeating)
            .unwrap();

        ev.signal(EVENT_SIGNALED, EVENT_SIGNALED);

        let p1 = mb.try_pop().unwrap();
        let p2 = mb.try_pop().unwrap();
        let kinds = [(p1.key, p1.kind), (p2.key, p2.kind)];
        assert!(kinds.contains(&(100, MailboxPacketKind::SignalOnce)));
        assert!(kinds.contains(&(200, MailboxPacketKind::SignalRepeating)));
        assert!(mb.try_pop().is_err());

        ev.signal(EVENT_SIGNALED, EVENT_SIGNALED);
        let p3 = mb.try_pop().unwrap();
        assert_eq!(p3.kind, MailboxPacketKind::SignalRepeating);
        assert_eq!(p3.key, 200);
        assert!(mb.try_pop().is_err());
    }

    #[test]
    fn cancel_repeating_vs_deliver_race() {
        use std::{sync::Barrier, thread};

        const ITERATIONS: u32 = 50_000;

        for iteration in 0..ITERATIONS {
            let mb = Mailbox::new();
            let ev = Event::new();
            let (target, koid) = target_event(&ev);
            mb.subscribe(&target, koid, 1, EVENT_SIGNALED, AsyncMode::Repeating)
                .unwrap();

            let barrier = Arc::new(Barrier::new(2));

            let mb_c = mb.clone();
            let barrier_c = barrier.clone();
            let canceller = thread::spawn(move || {
                barrier_c.wait();
                mb_c.cancel_subscription(koid, 1);
            });

            let ev_s = ev.clone();
            let barrier_s = barrier.clone();
            let signaller = thread::spawn(move || {
                barrier_s.wait();
                ev_s.signal(EVENT_SIGNALED, EVENT_SIGNALED);
            });

            canceller.join().unwrap();
            signaller.join().unwrap();

            // После cancel'а никаких будущих пакетов: очередь
            // максимум один пакет (cancel опоздал) либо пуста.
            let queue_len = mb.inner.with_lock(|inner| inner.queue.len());
            assert!(
                queue_len <= 1,
                "iteration {iteration}: лишний пакет в очереди ({queue_len})"
            );

            // Убедиться, что после cancel'а ev.signal больше пакетов
            // не приносит - observer должен быть отвязан.
            ev.signal(EVENT_SIGNALED, EVENT_SIGNALED);
            ev.signal(EVENT_SIGNALED, EVENT_SIGNALED);
            let queue_len_after = mb.inner.with_lock(|inner| inner.queue.len());
            assert_eq!(
                queue_len_after, queue_len,
                "iteration {iteration}: пакет после cancel'а ({queue_len_after} > {queue_len})"
            );
        }
    }
}
