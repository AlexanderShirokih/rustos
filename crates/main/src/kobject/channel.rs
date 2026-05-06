//! `ChannelEndpoint` KO: bounded message passing с трансфером handle'ов.
//!
//! Канал - пара `Arc<ChannelEndpoint>`, каждый со своей inbound-очередью.
//! `write` помещает сообщение в очередь *парного* эндпоинта (адресата);
//! `read` достаёт из *своей*. Каждый эндпоинт - самостоятельный KO с
//! сигналами `READABLE` / `PEER_CLOSED`.

use alloc::{
    collections::VecDeque,
    sync::{Arc, Weak},
    vec::{Drain, Vec},
};

use collections::{LockCell, MutexCell};

use super::{errors::IpcError, handle::Handle, wait::SignalState};

/// Сигнал "в inbound-очереди есть хотя бы одно сообщение".
pub const CHANNEL_READABLE: u32 = 1 << 0;
/// Сигнал "парный эндпоинт закрыт". Поднимается ровно один раз.
pub const CHANNEL_PEER_CLOSED: u32 = 1 << 1;

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
pub struct ChannelEndpoint {
    inner: MutexCell<EndpointInner>,
    signals: SignalState,
    peer: MutexCell<Weak<ChannelEndpoint>>,
}

impl ChannelEndpoint {
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
            signals: SignalState::new(0),
            peer: MutexCell::new(Weak::new()),
        });
        let b = Arc::new(Self {
            inner: MutexCell::new(EndpointInner::with_capacity(cap)),
            signals: SignalState::new(0),
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
        let peer = self
            .peer
            .with_lock(|p| p.upgrade())
            .ok_or(IpcError::PeerClosed)?;

        peer.inner.with_lock(|inner| {
            if inner.queue.len() >= inner.capacity {
                return Err(IpcError::ShouldWait);
            }
            inner.queue.push_back(msg);
            // Поднимаем READABLE под тем же локом, что и push: иначе
            // reader, успевший между push и signal вычитать всё в ноль,
            // мог бы очистить бит уже после нашего set'а - и оставить
            // непустую очередь без сигнала.
            //
            // PEER_CLOSED здесь не трогаем - им владеет только Drop.
            peer.signals.signal(CHANNEL_READABLE, 0);
            Ok(())
        })
    }

    /// Достаёт сообщение из своей inbound-очереди.
    ///
    /// - `ShouldWait`: очередь пуста, peer ещё жив.
    /// - `PeerClosed`: очередь пуста и peer закрыт.
    pub fn read(&self) -> Result<Message, IpcError> {
        let msg = self.inner.with_lock(|inner| {
            let msg = inner.queue.pop_front();
            // Очищаем READABLE атомарно с наблюдением `is_empty`: если
            // отпустить лок до signal'а, конкурентный writer может
            // успеть запушить новое сообщение и поднять бит, а наш
            // последующий clear перетёр бы его при непустой очереди -
            // и waiter'ы остались бы спать над уже доступным payload'ом.
            if msg.is_some() && inner.queue.is_empty() {
                self.signals.signal(0, CHANNEL_READABLE);
            }
            msg
        });

        match msg {
            Some(m) => Ok(m),
            None => {
                if self.signals.peek() & CHANNEL_PEER_CLOSED != 0 {
                    Err(IpcError::PeerClosed)
                } else {
                    Err(IpcError::ShouldWait)
                }
            }
        }
    }

    /// Возвращает сигнальное состояние (для интеграции с `object_wait_one`).
    pub fn signals(&self) -> &SignalState {
        &self.signals
    }
}

impl Drop for ChannelEndpoint {
    fn drop(&mut self) {
        if let Some(peer) = self.peer.with_lock(|p| p.upgrade()) {
            peer.signals.signal(CHANNEL_PEER_CLOSED, 0);
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
        let (a, b) = ChannelEndpoint::create_pair(4);
        a.write(payload(b"hello")).unwrap();
        let got = b.read().unwrap();
        assert_eq!(got.bytes(), b"hello");
        assert_eq!(got.handles_count(), 0);
    }

    #[test]
    fn read_empty_returns_should_wait() {
        let (_a, b) = ChannelEndpoint::create_pair(4);
        assert_eq!(b.read().unwrap_err(), IpcError::ShouldWait);
    }

    #[test]
    fn write_into_full_queue_returns_should_wait() {
        let (a, b) = ChannelEndpoint::create_pair(2);
        a.write(payload(b"1")).unwrap();
        a.write(payload(b"2")).unwrap();
        assert_eq!(a.write(payload(b"3")).unwrap_err(), IpcError::ShouldWait);

        b.read().unwrap();
        a.write(payload(b"3")).unwrap();
    }

    #[test]
    fn drop_peer_signals_peer_closed_and_returns_peer_closed() {
        let (a, b) = ChannelEndpoint::create_pair(4);

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
        let (a, b) = ChannelEndpoint::create_pair(4);
        drop(b);
        assert_eq!(a.write(payload(b"x")).unwrap_err(), IpcError::PeerClosed);
    }

    #[test]
    fn pending_messages_drain_after_peer_closes() {
        // Если peer закрылся, но в нашей очереди остались сообщения -
        // мы должны иметь возможность их вычитать; PeerClosed - только
        // когда очередь окончательно опустеет.
        let (a, b) = ChannelEndpoint::create_pair(4);
        a.write(payload(b"first")).unwrap();
        a.write(payload(b"second")).unwrap();
        drop(a);

        assert_eq!(b.read().unwrap().bytes(), b"first");
        assert_eq!(b.read().unwrap().bytes(), b"second");
        assert_eq!(b.read().unwrap_err(), IpcError::PeerClosed);
    }

    #[test]
    fn read_signals_become_consistent_after_drain() {
        let (a, b) = ChannelEndpoint::create_pair(4);
        a.write(payload(b"x")).unwrap();
        assert_eq!(b.peek_signals() & CHANNEL_READABLE, CHANNEL_READABLE);
        b.read().unwrap();
        assert_eq!(b.peek_signals() & CHANNEL_READABLE, 0);
    }

    #[test]
    fn waiter_woken_on_first_write() {
        let (a, b) = ChannelEndpoint::create_pair(4);
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
        let (a, b) = ChannelEndpoint::create_pair(4);
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
            let (writer_end, reader_end) = ChannelEndpoint::create_pair(4);
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

        let (writer_end, reader_end) = ChannelEndpoint::create_pair(2);
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
        let (sender_end, receiver_end) = ChannelEndpoint::create_pair(4);

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
}
