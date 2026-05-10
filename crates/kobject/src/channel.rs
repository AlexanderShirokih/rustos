//! `Channel` KO: bounded message passing с трансфером handle'ов.
//!
//! Канал - пара `Arc<Channel>`, каждый со своей inbound-очередью.
//! `write` помещает сообщение в очередь *парного* эндпоинта (адресата);
//! `read` достаёт из *своей*. Каждый эндпоинт - самостоятельный KO с
//! сигналами `READABLE` / `PEER_CLOSED`.

use alloc::{
    collections::VecDeque,
    sync::{Arc, Weak},
    vec::{Drain, Vec},
};

use collections::{LockCell, MutexCell};

use super::{
    errors::IpcError,
    handle::Handle,
    wait::{SignalSource, SignalState},
};

/// Сигнал "в inbound-очереди есть хотя бы одно сообщение".
pub const CHANNEL_READABLE: u32 = 1 << 0;
/// Сигнал "парный эндпоинт закрыт". Поднимается ровно один раз.
pub const CHANNEL_PEER_CLOSED: u32 = 1 << 1;
/// Сигнал "в очереди peer'а есть место хотя бы под одно сообщение".
/// Снимается при заполнении очереди peer'а до края, поднимается обратно
/// при освобождении слота (read со стороны peer'а). Дропается peer'ом
/// в момент закрытия (`PEER_CLOSED` делает write бессмысленным).
pub const CHANNEL_WRITABLE: u32 = 1 << 2;

/// Максимальный размер inline-данных в одном сообщении.
pub const MESSAGE_INLINE_MAX: usize = 256;
/// Максимум handle'ов, переносимых одним сообщением.
pub const MESSAGE_MAX_HANDLES: usize = 4;
/// Стартовая ёмкость очереди эндпоинта.
pub const DEFAULT_CHANNEL_CAPACITY: usize = 16;

/// Сообщение канала: payload-байты + до `MESSAGE_MAX_HANDLES` handle'ов.
///
/// `bytes` и `handles` - два owning-`Vec`, поэтому `Message` сам по
/// себе занимает в стеке только два "толстых указателя" (≈ 48 B), и
/// его перемещение между стеком, очередью канала и потоком-получателем
/// сводится к копированию этих указателей. Payload и handle-list
/// аллоцируются на куче ровно по факту использования: пустое сообщение
/// не выполняет ни одного malloc, а `from_bytes(N)` делает ровно одну
/// аллокацию точного размера `N` (без зануления буфера).
pub struct Message {
    bytes: Vec<u8>,
    handles: Vec<Handle>,
}

impl Message {
    pub fn new() -> Self {
        Self {
            bytes: Vec::new(),
            handles: Vec::new(),
        }
    }

    /// Конструирует сообщение из готового слайса байт.
    ///
    /// Делает одну аллокацию ёмкостью ровно `bytes.len()` и единственный
    /// `memcpy` - без промежуточного зануления буфера, как было бы в
    /// варианте с inline-массивом.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, IpcError> {
        if bytes.len() > MESSAGE_INLINE_MAX {
            return Err(IpcError::MessageTooBig);
        }
        Ok(Self {
            bytes: bytes.to_vec(),
            handles: Vec::new(),
        })
    }

    /// Дописывает handle к сообщению.
    pub fn push_handle(&mut self, handle: Handle) -> Result<(), IpcError> {
        if self.handles.len() >= MESSAGE_MAX_HANDLES {
            return Err(IpcError::MessageTooBig);
        }
        // Резервируем место под все возможные handle'ы один раз -
        // последующие `push_handle` идут без realloc.
        if self.handles.capacity() == 0 {
            self.handles.reserve_exact(MESSAGE_MAX_HANDLES);
        }
        self.handles.push(handle);
        Ok(())
    }

    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    pub fn handles_count(&self) -> usize {
        self.handles.len()
    }

    /// Извлекает все handle'ы для регистрации в таблице получателя.
    /// Возвращает стандартный `Vec::drain` - оставшиеся при раннем
    /// отказе элементы корректно дропаются вместе с итератором.
    pub fn drain_handles(&mut self) -> Drain<'_, Handle> {
        self.handles.drain(..)
    }
}

impl Default for Message {
    fn default() -> Self {
        Self::new()
    }
}

impl core::fmt::Debug for Message {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Message")
            .field("bytes_len", &self.bytes.len())
            .field("handles_len", &self.handles.len())
            .finish_non_exhaustive()
    }
}

struct EndpointInner {
    queue: VecDeque<Message>,
    capacity: usize,
}

impl EndpointInner {
    fn with_capacity(capacity: usize) -> Self {
        Self {
            // Префиксная аллокация на полную ёмкость очереди - push_back
            // в hot path никогда не делает realloc.
            queue: VecDeque::with_capacity(capacity),
            capacity,
        }
    }
}

/// Один из двух эндпоинтов канала. KO; ходит между процессами через `Handle::TRANSFER`.
pub struct Channel {
    inner: MutexCell<EndpointInner>,
    signals: SignalState,
    peer: MutexCell<Weak<Channel>>,
}

impl Channel {
    /// Создаёт пару связанных эндпоинтов с общей `capacity` для каждой
    /// inbound-очереди. `capacity == 0` интерпретируется как
    /// [`DEFAULT_CHANNEL_CAPACITY`].
    pub fn create_pair(capacity: usize) -> (Arc<Self>, Arc<Self>) {
        let cap = if capacity == 0 {
            DEFAULT_CHANNEL_CAPACITY
        } else {
            capacity
        };

        let a = Arc::new(Self {
            inner: MutexCell::new(EndpointInner::with_capacity(cap)),
            signals: SignalState::new(CHANNEL_WRITABLE),
            peer: MutexCell::new(Weak::new()),
        });
        let b = Arc::new(Self {
            inner: MutexCell::new(EndpointInner::with_capacity(cap)),
            signals: SignalState::new(CHANNEL_WRITABLE),
            peer: MutexCell::new(Weak::new()),
        });

        a.peer.with_lock(|p| *p = Arc::downgrade(&b));
        b.peer.with_lock(|p| *p = Arc::downgrade(&a));

        (a, b)
    }

    /// Снимок текущих сигналов (lockless).
    pub fn peek_signals(&self) -> u32 {
        self.signals.peek()
    }

    /// Помещает сообщение в inbound-очередь *парного* эндпоинта.
    ///
    /// - `PeerClosed`: парный эндпоинт уже дропнут.
    /// - `ShouldWait`: очередь адресата заполнена.
    pub fn write(&self, msg: Message) -> Result<(), IpcError> {
        // Тонкий wrapper над `try_write` с infallible build: внутренний
        // `Result<_, Infallible>` всегда `Ok`, поэтому inner-Err
        // невозможна.
        let mut slot = Some(msg);
        match self.try_write::<core::convert::Infallible>(|| {
            Ok(slot.take().expect("build closure invoked exactly once"))
        }) {
            Ok(Ok(())) => Ok(()),
            Ok(Err(_)) => unreachable!("Infallible cannot be constructed"),
            Err(e) => Err(e),
        }
    }

    /// Атомарная ветка write для callers, которым нужно отложить
    /// материализацию `Message` до момента, когда peer.queue точно
    /// имеет место.
    ///
    /// `build` вызывается ровно один раз - и **только** при наличии
    /// слота в очереди peer'а; на full-queue / closed-peer пути closure
    /// не выполняется (важно для syscall-handler'а: при ShouldWait
    /// user-handle'ы не должны быть изъяты из source-table).
    ///
    /// Возврат:
    /// - внешний `Err` - канал-уровневая ошибка (peer закрыт, нет места);
    /// - внутренний `Err` - ошибка построения сообщения; очередь и
    ///   сигналы не модифицируются.
    pub fn try_write<E>(
        &self,
        build: impl FnOnce() -> Result<Message, E>,
    ) -> Result<Result<(), E>, IpcError> {
        let peer = self
            .peer
            .with_lock(|p| p.upgrade())
            .ok_or(IpcError::PeerClosed)?;

        peer.inner.with_lock(|inner| {
            if inner.queue.len() >= inner.capacity {
                return Err(IpcError::ShouldWait);
            }
            let msg = match build() {
                Ok(m) => m,
                Err(e) => return Ok(Err(e)),
            };
            inner.queue.push_back(msg);
            // Сигналы поднимаем под тем же локом, что и push: иначе
            // конкурентный reader/writer мог бы оставить бит и очередь в
            // несогласованном состоянии (см. соответствующий
            // тест-регрессию `split_pop_and_clear_loses_readable_bit`).
            // PEER_CLOSED здесь не трогаем - им владеет только Drop.
            peer.signals.signal(CHANNEL_READABLE, 0);
            if inner.queue.len() == inner.capacity {
                self.signals.signal(0, CHANNEL_WRITABLE);
            }
            Ok(Ok(()))
        })
    }

    /// Достаёт сообщение из своей inbound-очереди.
    ///
    /// - `ShouldWait`: очередь пуста, peer ещё жив.
    /// - `PeerClosed`: очередь пуста и peer закрыт.
    pub fn read(&self) -> Result<Message, IpcError> {
        match self.try_read::<core::convert::Infallible>(|_| Ok(())) {
            Ok(Ok(msg)) => Ok(msg),
            Ok(Err(_)) => unreachable!("Infallible cannot be constructed"),
            Err(e) => Err(self.map_should_wait_to_peer_closed(e)),
        }
    }

    /// Cap-aware вариант [`Self::read`]: peek первой записи под локом,
    /// при превышении любого из лимитов - `BufferTooSmall` без pop'а
    /// (сообщение остаётся в очереди, READABLE сохраняется).
    pub fn read_with_caps(
        &self,
        bytes_cap: usize,
        handles_cap: usize,
    ) -> Result<Message, IpcError> {
        match self.try_read::<IpcError>(|msg| {
            if msg.bytes().len() > bytes_cap || msg.handles_count() > handles_cap {
                Err(IpcError::BufferTooSmall)
            } else {
                Ok(())
            }
        }) {
            Ok(Ok(msg)) => Ok(msg),
            Ok(Err(e)) => Err(e),
            Err(e) => Err(self.map_should_wait_to_peer_closed(e)),
        }
    }

    /// `ShouldWait` при закрытом peer'е семантически равен `PeerClosed`.
    fn map_should_wait_to_peer_closed(&self, e: IpcError) -> IpcError {
        if matches!(e, IpcError::ShouldWait) && self.signals.peek() & CHANNEL_PEER_CLOSED != 0 {
            IpcError::PeerClosed
        } else {
            e
        }
    }

    /// Атомарный read: внутри `self.inner`-лока вызывает `finalize` с
    /// `&mut Message` головы очереди. На `Err` из `finalize` сообщение
    /// остаётся в очереди и сигналы не трогаются (вызывающий получает
    /// `Ok(Err(_))`). На `Ok` - сообщение pop'ится, READABLE/WRITABLE
    /// атомарно обновляются.
    ///
    /// Внешний `Err` - канал-уровневые состояния (`ShouldWait` /
    /// `PeerClosed`).
    ///
    /// `finalize` может изъять handle'ы (`drain_handles`), вернуть их
    /// обратно (`push_handle`) и т.п. - ровно одна точка коммита, без
    /// race-окон между peek и pop.
    pub fn try_read<E>(
        &self,
        finalize: impl FnOnce(&mut Message) -> Result<(), E>,
    ) -> Result<Result<Message, E>, IpcError> {
        self.inner.with_lock(|inner| {
            let Some(front) = inner.queue.front_mut() else {
                return Err(IpcError::ShouldWait);
            };
            if let Err(e) = finalize(front) {
                return Ok(Err(e));
            }
            let was_full = inner.queue.len() == inner.capacity;
            let msg = inner.queue.pop_front().expect("front observed above");
            // READABLE/WRITABLE - под self.inner-локом: иначе concurrent
            // writer успел бы заполнить очередь обратно и оставить
            // WRITABLE при полной queue, либо clear READABLE поверх
            // непустой очереди.
            if inner.queue.is_empty() {
                self.signals.signal(0, CHANNEL_READABLE);
            }
            if was_full && let Some(peer) = self.peer.with_lock(|p| p.upgrade()) {
                peer.signals.signal(CHANNEL_WRITABLE, 0);
            }
            Ok(Ok(msg))
        })
    }

    /// Возвращает сигнальное состояние (для интеграции с `object_wait_one`).
    pub fn signals(&self) -> &SignalState {
        &self.signals
    }
}

impl SignalSource for Channel {
    fn signals(&self) -> &SignalState {
        &self.signals
    }
}

impl Drop for Channel {
    fn drop(&mut self) {
        if let Some(peer) = self.peer.with_lock(|p| p.upgrade()) {
            // Сбрасываем WRITABLE у peer'а вместе с поднятием
            // PEER_CLOSED: write со стороны peer'а теперь обречён
            // вернуть PeerClosed, и WRITABLE-сигнал бессмыслен.
            peer.signals.signal(CHANNEL_PEER_CLOSED, CHANNEL_WRITABLE);
        }
        // Висячие сообщения в self.inner.queue дропаются вместе с self;
        // их handle'ы автоматически закрываются (Arc -> 0).
    }
}

#[cfg(test)]
mod tests {
    use alloc::sync::Arc;

    use super::{
        super::{
            event::Event, handle::Handle, handle_table::HandleTable, object::KObject,
            rights::Rights, wait::MockWaker,
        },
        *,
    };

    /// Собирает простое сообщение из строкового payload без handle'ов.
    fn payload(s: &[u8]) -> Message {
        Message::from_bytes(s).expect("payload fits")
    }

    #[test]
    fn write_then_read_round_trip() {
        let (a, b) = Channel::create_pair(4);
        a.write(payload(b"hello")).unwrap();
        let got = b.read().unwrap();
        assert_eq!(got.bytes(), b"hello");
        assert_eq!(got.handles_count(), 0);
    }

    #[test]
    fn read_empty_returns_should_wait() {
        let (_a, b) = Channel::create_pair(4);
        assert_eq!(b.read().unwrap_err(), IpcError::ShouldWait);
    }

    #[test]
    fn write_into_full_queue_returns_should_wait() {
        let (a, b) = Channel::create_pair(2);
        a.write(payload(b"1")).unwrap();
        a.write(payload(b"2")).unwrap();
        assert_eq!(a.write(payload(b"3")).unwrap_err(), IpcError::ShouldWait);

        b.read().unwrap();
        a.write(payload(b"3")).unwrap();
    }

    #[test]
    fn drop_peer_signals_peer_closed_and_returns_peer_closed() {
        let (a, b) = Channel::create_pair(4);

        // Зарегистрируем waiter на сигнал PEER_CLOSED у b.
        let waker = MockWaker::new();
        b.signals
            .register_waiter(CHANNEL_PEER_CLOSED, waker.clone());

        drop(a);

        assert!(waker.was_woken());
        assert_eq!(b.peek_signals() & CHANNEL_PEER_CLOSED, CHANNEL_PEER_CLOSED);
        // Очередь пуста + peer закрыт => PeerClosed.
        assert_eq!(b.read().unwrap_err(), IpcError::PeerClosed);
    }

    #[test]
    fn write_after_peer_closed_returns_peer_closed() {
        let (a, b) = Channel::create_pair(4);
        drop(b);
        assert_eq!(a.write(payload(b"x")).unwrap_err(), IpcError::PeerClosed);
    }

    #[test]
    fn pending_messages_drain_after_peer_closes() {
        // Если peer закрылся, но в нашей очереди остались сообщения -
        // мы должны иметь возможность их вычитать; PeerClosed - только
        // когда очередь окончательно опустеет.
        let (a, b) = Channel::create_pair(4);
        a.write(payload(b"first")).unwrap();
        a.write(payload(b"second")).unwrap();
        drop(a);

        assert_eq!(b.read().unwrap().bytes(), b"first");
        assert_eq!(b.read().unwrap().bytes(), b"second");
        assert_eq!(b.read().unwrap_err(), IpcError::PeerClosed);
    }

    #[test]
    fn read_signals_become_consistent_after_drain() {
        let (a, b) = Channel::create_pair(4);
        a.write(payload(b"x")).unwrap();
        assert_eq!(b.peek_signals() & CHANNEL_READABLE, CHANNEL_READABLE);
        b.read().unwrap();
        assert_eq!(b.peek_signals() & CHANNEL_READABLE, 0);
    }

    #[test]
    fn waiter_woken_on_first_write() {
        let (a, b) = Channel::create_pair(4);
        let waker = MockWaker::new();
        b.signals.register_waiter(CHANNEL_READABLE, waker.clone());
        assert!(!waker.was_woken());

        a.write(payload(b"data")).unwrap();
        assert!(waker.was_woken());
    }

    /// Регрессия на race-window между `pop_front` и `signal(0, READABLE)`
    /// в `read`. Если из очереди вычитан не последний элемент, бит
    /// `READABLE` обязан остаться поднятым - иначе любой waiter,
    /// зарегистрированный сразу после успешного `read`, увидит "нет
    /// данных" при непустой очереди и уйдёт спать поверх уже
    /// доступного payload'а.
    #[test]
    fn read_keeps_readable_when_queue_non_empty() {
        let (a, b) = Channel::create_pair(4);
        a.write(payload(b"1")).unwrap();
        a.write(payload(b"2")).unwrap();
        a.write(payload(b"3")).unwrap();

        assert_eq!(b.peek_signals() & CHANNEL_READABLE, CHANNEL_READABLE);

        // Вычитываем 2 из 3 - бит должен оставаться выставленным.
        b.read().unwrap();
        assert_eq!(b.peek_signals() & CHANNEL_READABLE, CHANNEL_READABLE);
        b.read().unwrap();
        assert_eq!(b.peek_signals() & CHANNEL_READABLE, CHANNEL_READABLE);

        // Снимаем последний - бит должен очиститься атомарно с тем же
        // pop'ом, не оставляя окна, в которое вклинится конкурентный
        // writer.
        b.read().unwrap();
        assert_eq!(b.peek_signals() & CHANNEL_READABLE, 0);
    }

    /// Параллельные `read` и `write` на одном эндпоинте не должны
    /// оставлять непустую очередь без `READABLE`.
    ///
    /// Здесь два потока через `Barrier` синхронно стартуют
    /// `read` (вычитывающий единственный pre-fill'нутый элемент) и
    /// `write` (запушающий новый). После завершения обеих операций
    /// единственно оставшееся сообщение должно наблюдаться через
    /// `READABLE`.
    #[test]
    fn concurrent_write_during_read_keeps_readable_bit() {
        extern crate std;
        use std::{sync::Barrier, thread};

        const ITERATIONS: u32 = 50_000;

        for iteration in 0..ITERATIONS {
            let (writer_end, reader_end) = Channel::create_pair(4);
            // Pre-fill ровно одним сообщением: bit=1, queue=[first].
            // Reader должен вычитать его и наблюдать queue.is_empty()==true,
            // пока writer добавляет следующее сообщение.
            writer_end
                .write(Message::from_bytes(b"first").unwrap())
                .unwrap();

            let barrier = Arc::new(Barrier::new(2));

            let barrier_w = barrier.clone();
            let writer_for_thread = writer_end.clone();
            let writer = thread::spawn(move || {
                barrier_w.wait();
                writer_for_thread
                    .write(Message::from_bytes(b"second").unwrap())
                    .unwrap();
            });

            barrier.wait();
            let first = reader_end.read().expect("first read succeeds");

            writer.join().unwrap();

            assert_eq!(first.bytes(), b"first");

            // В очереди ровно одно сообщение, значит READABLE выставлен.
            assert_eq!(
                reader_end.peek_signals() & CHANNEL_READABLE,
                CHANNEL_READABLE,
                "iteration {iteration}: READABLE потерян после race write/read \
                 (бит был очищен поверх непустой очереди)"
            );

            // Дочитываем "second" и убеждаемся, что бит снимается
            // ровно в этот момент.
            let second = reader_end.read().expect("second read succeeds");
            assert_eq!(second.bytes(), b"second");
            assert_eq!(reader_end.peek_signals() & CHANNEL_READABLE, 0);
        }
    }

    /// Разнесение `pop` и `clear` по разным критическим секциям
    /// теряет `READABLE`, если между ними проходит `write`.
    /// Waiter, зарегистрированный после такого clear, не видит
    /// уже доступное сообщение.
    #[test]
    fn split_pop_and_clear_loses_readable_bit() {
        extern crate std;
        use std::{sync::Barrier, thread};

        let (writer_end, reader_end) = Channel::create_pair(2);
        writer_end
            .write(Message::from_bytes(b"first").unwrap())
            .unwrap();

        let pre_clear = Arc::new(Barrier::new(2));
        let post_push = Arc::new(Barrier::new(2));

        let pre_clear_w = pre_clear.clone();
        let post_push_w = post_push.clone();
        let writer_for_thread = writer_end.clone();
        let writer = thread::spawn(move || {
            // Ждём, пока reader сделает pop, но ещё не очистит бит.
            pre_clear_w.wait();
            writer_for_thread
                .write(Message::from_bytes(b"second").unwrap())
                .unwrap();
            post_push_w.wait();
        });

        // pop в одной критсекции, clear - отдельно.
        let (msg, now_empty) = reader_end.inner.with_lock(|inner| {
            let m = inner.queue.pop_front();
            (m, inner.queue.is_empty())
        });
        assert_eq!(msg.unwrap().bytes(), b"first");
        assert!(now_empty);

        // В это окно writer добавляет сообщение и поднимает бит.
        pre_clear.wait();
        post_push.wait();

        // Применяем clear отдельно от pop.
        if now_empty {
            reader_end.signals.signal(0, CHANNEL_READABLE);
        }

        writer.join().unwrap();

        // Очередь содержит msg2, но бит снят: waiter не увидит сообщение.
        assert_eq!(reader_end.peek_signals() & CHANNEL_READABLE, 0);
        let waker = MockWaker::new();
        reader_end
            .signals()
            .register_waiter(CHANNEL_READABLE, waker.clone());
        assert!(
            !waker.was_woken(),
            "waiter не должен сработать без READABLE"
        );

        // Сообщение остаётся в очереди при погашенном бите.
        assert_eq!(reader_end.read().unwrap().bytes(), b"second");
    }

    #[test]
    fn message_too_big_rejected() {
        let buf = [0u8; MESSAGE_INLINE_MAX + 1];
        assert_eq!(
            Message::from_bytes(&buf).unwrap_err(),
            IpcError::MessageTooBig
        );
    }

    #[test]
    fn handles_overflow_rejected() {
        let mut msg = Message::new();
        for _ in 0..MESSAGE_MAX_HANDLES {
            msg.push_handle(make_event_handle()).unwrap();
        }
        let res = msg.push_handle(make_event_handle());
        assert_eq!(res.unwrap_err(), IpcError::MessageTooBig);
    }

    #[test]
    fn dropping_message_with_handles_releases_arcs() {
        // Message - strong_count объекта должен упасть до 0.
        let obj = Event::new();
        let weak = Arc::downgrade(&obj);
        let handle = Handle::new(KObject::Event(obj), Rights::WAIT);

        let mut msg = Message::new();
        msg.push_handle(handle).unwrap();
        drop(msg);
        assert!(weak.upgrade().is_none());
    }

    #[test]
    fn handle_transfer_between_tables_via_channel() {
        // Имитация атомарного transfer-протокола: source-таблица под
        // локом отдаёт handle в Message -> write в канал; получатель
        // читает Message и регистрирует handle в своей таблице.
        let (sender_end, receiver_end) = Channel::create_pair(4);

        let mut sender_table = HandleTable::new();
        let mut receiver_table = HandleTable::new();

        // Источник: помещаем в свою таблицу handle на Event.
        let event = Event::new();
        let koid_before = KObject::Event(event.clone()).koid();
        let event_id = sender_table
            .insert(Handle::new(
                KObject::Event(event),
                Rights::WAIT | Rights::TRANSFER,
            ))
            .unwrap();

        // "Системный" write_with_handles: take ownership под локом source
        // -> упаковать в Message -> отдать.
        let removed = sender_table.remove(event_id).unwrap();
        assert_eq!(sender_table.live_count(), 0);

        let mut msg = Message::from_bytes(b"reply-here").unwrap();
        msg.push_handle(removed).unwrap();
        sender_end.write(msg).unwrap();

        // Адресат читает и регистрирует handle в своей таблице.
        let mut payload = receiver_end.read().unwrap();
        assert_eq!(payload.bytes(), b"reply-here");
        let drained: alloc::vec::Vec<_> = payload.drain_handles().collect();
        assert_eq!(drained.len(), 1);

        let mut new_id = None;
        for h in drained {
            new_id = Some(receiver_table.insert(h).unwrap());
        }
        let new_id = new_id.unwrap();

        // KO жив, его представляет новый handle с теми же правами.
        let h = receiver_table.get(new_id, Rights::WAIT).unwrap();
        assert_eq!(h.koid(), koid_before);
        assert_eq!(receiver_table.live_count(), 1);
    }

    fn make_event_handle() -> Handle {
        Handle::new(KObject::Event(Event::new()), Rights::WAIT)
    }

    #[test]
    fn writable_initial_state_set_for_both_endpoints() {
        let (a, b) = Channel::create_pair(2);
        assert_eq!(a.peek_signals() & CHANNEL_WRITABLE, CHANNEL_WRITABLE);
        assert_eq!(b.peek_signals() & CHANNEL_WRITABLE, CHANNEL_WRITABLE);
    }

    #[test]
    fn writable_cleared_when_peer_queue_full() {
        let (a, b) = Channel::create_pair(2);
        assert_eq!(a.peek_signals() & CHANNEL_WRITABLE, CHANNEL_WRITABLE);

        a.write(payload(b"1")).unwrap();
        assert_eq!(a.peek_signals() & CHANNEL_WRITABLE, CHANNEL_WRITABLE);

        a.write(payload(b"2")).unwrap();
        assert_eq!(a.peek_signals() & CHANNEL_WRITABLE, 0);

        // Drain peer и проверяем восстановление WRITABLE.
        b.read().unwrap();
        assert_eq!(a.peek_signals() & CHANNEL_WRITABLE, CHANNEL_WRITABLE);
    }

    #[test]
    fn writable_raised_for_peer_when_queue_drains() {
        let (a, b) = Channel::create_pair(1);
        a.write(payload(b"x")).unwrap();
        assert_eq!(a.peek_signals() & CHANNEL_WRITABLE, 0);
        b.read().unwrap();
        assert_eq!(a.peek_signals() & CHANNEL_WRITABLE, CHANNEL_WRITABLE);
    }

    #[test]
    fn writable_cleared_on_peer_drop() {
        let (a, b) = Channel::create_pair(4);
        assert_eq!(a.peek_signals() & CHANNEL_WRITABLE, CHANNEL_WRITABLE);
        drop(b);
        assert_eq!(a.peek_signals() & CHANNEL_WRITABLE, 0);
        assert_eq!(a.peek_signals() & CHANNEL_PEER_CLOSED, CHANNEL_PEER_CLOSED);
    }

    #[test]
    fn writable_waiter_woken_when_queue_drains() {
        let (a, b) = Channel::create_pair(1);
        a.write(payload(b"x")).unwrap();
        assert_eq!(a.peek_signals() & CHANNEL_WRITABLE, 0);

        let waker = MockWaker::new();
        a.signals.register_waiter(CHANNEL_WRITABLE, waker.clone());
        assert!(!waker.was_woken());

        b.read().unwrap();
        assert!(waker.was_woken());
        assert_eq!(waker.observed() & CHANNEL_WRITABLE, CHANNEL_WRITABLE);
    }

    #[test]
    fn try_write_skips_build_when_queue_full() {
        use core::cell::Cell;

        let (a, b) = Channel::create_pair(1);
        a.write(payload(b"first")).unwrap();

        let invoked = Cell::new(false);
        let res = a.try_write::<core::convert::Infallible>(|| {
            invoked.set(true);
            Ok(payload(b"second"))
        });
        assert!(matches!(res, Err(IpcError::ShouldWait)));
        assert!(
            !invoked.get(),
            "build closure must be skipped on full queue"
        );

        // Очередь не модифицирована.
        let got = b.read().unwrap();
        assert_eq!(got.bytes(), b"first");
        assert!(b.read().is_err());
    }

    #[test]
    fn try_write_build_err_leaves_state_untouched() {
        let (a, b) = Channel::create_pair(2);
        let initial_signals = b.peek_signals();

        #[derive(Debug, PartialEq)]
        struct BuildFailed;
        let res = a.try_write::<BuildFailed>(|| Err(BuildFailed));
        assert!(matches!(res, Ok(Err(BuildFailed))));

        // Очередь у b пуста, READABLE не поднят, sender WRITABLE не сброшен.
        assert_eq!(b.peek_signals(), initial_signals);
        assert_eq!(a.peek_signals() & CHANNEL_WRITABLE, CHANNEL_WRITABLE);
        assert!(b.read().is_err());
    }

    #[test]
    fn read_with_caps_buffer_too_small_keeps_message() {
        let (a, b) = Channel::create_pair(4);
        a.write(payload(b"hello-world")).unwrap();
        assert_eq!(b.peek_signals() & CHANNEL_READABLE, CHANNEL_READABLE);

        let err = b.read_with_caps(4, 0).unwrap_err();
        assert_eq!(err, IpcError::BufferTooSmall);
        // READABLE сохранён: сообщение по-прежнему в очереди.
        assert_eq!(b.peek_signals() & CHANNEL_READABLE, CHANNEL_READABLE);

        // Полноценный read получает то же самое сообщение.
        let got = b.read().unwrap();
        assert_eq!(got.bytes(), b"hello-world");
    }

    #[test]
    fn read_with_caps_handles_too_few_keeps_message() {
        let (a, b) = Channel::create_pair(4);
        let mut msg = Message::from_bytes(b"x").unwrap();
        msg.push_handle(make_event_handle()).unwrap();
        a.write(msg).unwrap();

        let err = b.read_with_caps(64, 0).unwrap_err();
        assert_eq!(err, IpcError::BufferTooSmall);
        // Можно вычитать с правильным cap'ом.
        let got = b.read_with_caps(64, MESSAGE_MAX_HANDLES).unwrap();
        assert_eq!(got.handles_count(), 1);
    }

    /// Регрессия P1 (PR#32 codex review): finalize-Err в `try_read`
    /// должен оставлять сообщение в очереди со всеми handle'ами,
    /// которые closure успел изъять и вернуть через `push_handle`.
    /// Эмулирует "receiver-table is full"-сценарий: closure делает
    /// `drain_handles` -> "install fails" -> кладёт всё обратно
    /// через `push_handle` -> возвращает Err. После этого следующий
    /// успешный read обязан получить то же сообщение целиком.
    #[test]
    fn try_read_err_after_drain_restores_message() {
        let (a, b) = Channel::create_pair(2);
        let event = Event::new();
        let weak = Arc::downgrade(&event);
        let mut msg = Message::from_bytes(b"payload").unwrap();
        msg.push_handle(Handle::new(KObject::Event(event), Rights::WAIT))
            .unwrap();
        a.write(msg).unwrap();
        assert_eq!(b.peek_signals() & CHANNEL_READABLE, CHANNEL_READABLE);

        #[derive(Debug, PartialEq)]
        struct InstallFailed;
        let res = b.try_read::<InstallFailed>(|m| {
            let drained: alloc::vec::Vec<_> = m.drain_handles().collect();
            assert_eq!(drained.len(), 1);
            // Эмулируем неудачный install: возвращаем handle обратно в msg.
            for h in drained {
                m.push_handle(h).unwrap();
            }
            Err(InstallFailed)
        });
        assert!(matches!(res, Ok(Err(InstallFailed))));

        // Сообщение и его handle на месте: очередь не пуста, KO не закрыт.
        assert!(weak.upgrade().is_some());
        assert_eq!(b.peek_signals() & CHANNEL_READABLE, CHANNEL_READABLE);

        let got = b.read().unwrap();
        assert_eq!(got.bytes(), b"payload");
        assert_eq!(got.handles_count(), 1);
    }

    /// Регрессия P2 (PR#32 codex review): после `read` из полной
    /// очереди WRITABLE поднимается у peer'а атомарно с pop'ом, под
    /// тем же self.inner-локом. Race-сценарий: writer успевает
    /// заполнить очередь обратно между нашим pop'ом и signal'ом - и
    /// перетирает наш WRITABLE поверх полной queue.
    #[test]
    fn writable_not_set_after_concurrent_refill() {
        extern crate std;
        use std::{sync::Barrier, thread};

        const ITERATIONS: u32 = 50_000;
        for iteration in 0..ITERATIONS {
            // capacity=1: после первого write очередь полная и WRITABLE=0
            // у writer'а.
            let (writer_end, reader_end) = Channel::create_pair(1);
            writer_end.write(payload(b"first")).unwrap();
            assert_eq!(writer_end.peek_signals() & CHANNEL_WRITABLE, 0);

            let barrier = Arc::new(Barrier::new(2));

            let barrier_w = barrier.clone();
            let writer_for_thread = writer_end.clone();
            let writer = thread::spawn(move || {
                barrier_w.wait();
                // Сразу после reader.read() заполняем заново.
                while writer_for_thread.write(payload(b"second")).is_err() {
                    core::hint::spin_loop();
                }
            });

            barrier.wait();
            reader_end.read().unwrap();
            writer.join().unwrap();

            // На этом моменте: либо writer уже залил queue (тогда
            // WRITABLE=0 - корректно), либо reader выкатил signal до
            // refill'а (тогда WRITABLE могло быть 1 кратко, но к
            // моменту наблюдения writer уже залил - WRITABLE=0).
            // Если фикс не работает, WRITABLE может остаться поднятым
            // при полной queue: ошибка.
            let signals = writer_end.peek_signals();
            // Очередь точно полная (1 элемент, capacity=1) - WRITABLE=0.
            assert_eq!(
                signals & CHANNEL_WRITABLE,
                0,
                "iteration {iteration}: WRITABLE поднят при полной \
                 очереди (race между pop и peer.signal)"
            );

            // Дочитываем "second" - WRITABLE должен подняться.
            reader_end.read().unwrap();
            assert_eq!(
                writer_end.peek_signals() & CHANNEL_WRITABLE,
                CHANNEL_WRITABLE
            );
        }
    }
}
