//! Объявляет хостовый канал для тестов IPC-контрактов: клиентскую сторону [`MockChannel`] и
//! серверную [`MockListener`].
//!
//! [`pair`] отдаёт две стороны одного канала. Кадры встают в очередь под мьютексом, а ожидание
//! снимает condvar. Тест разводит стороны контракта по потокам и берёт ресурсы принятого кадра
//! набором [`MockResources`]. Кадр вызова приходит с каналом ответа [`MockReply`], ответ которого
//! получает ждущий вызыватель.

#![allow(
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::unwrap_in_result,
    clippy::panic,
    clippy::std_instead_of_core
)]

use std::{
    collections::VecDeque,
    marker::PhantomData,
    sync::{
        Arc, Condvar, Mutex, MutexGuard,
        atomic::{AtomicU32, AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

use ipc::{
    Channel, FrameBytes, Incoming, IntoWireValue, Listener, Outgoing, OutgoingParts,
    ReceivedResources, RecvError, ReplyChannel, Timeout, Unsent,
    wire::{Cap, IpcError, MessageBuf},
};
use syscall::{ObjectKind, kind};

mod async_channel;
mod complete;

pub use complete::complete;

/// Capability кадра как владельцы [`MockHandle`] по слотам. Вид результата задаёт вид поля
/// контракта, а фактический объект ядра мок не проверяет.
#[derive(Debug, Default)]
pub struct MockResources {
    handles: Vec<Option<MockHandle<kind::Any>>>,
    reply: Option<MockReply>,
}

impl ReceivedResources for MockResources {
    type Handle<K: ObjectKind> = MockHandle<K>;
    type Reply = MockReply;

    fn take_handle<K: ObjectKind>(&mut self, index: u8) -> Option<MockHandle<K>> {
        let handle = self.handles.get_mut(usize::from(index))?.take()?;
        Some(handle.retype())
    }

    fn put_handle<K: ObjectKind>(&mut self, handle: MockHandle<K>) -> Option<u8> {
        let index = u8::try_from(self.handles.len()).ok()?;
        self.handles.push(Some(handle.retype()));
        Some(index)
    }

    fn reply_channel(handle: MockHandle<kind::Reply>) -> Option<MockReply> {
        let slot = take_forwarded(handle.raw())?;
        Some(MockReply { slot: Some(slot) })
    }

    fn take_reply(&mut self) -> Option<MockReply> {
        self.reply.take()
    }
}

/// Тестовый владелец capability вида `K`, построенный из числа.
///
/// Номер к записи ядра отношения не имеет. Разрушение владельца закрывает capability: владелец из
/// [`MockHandle::tracked`] считает закрытие в своём счётчике, а владелец канала ответа, записанного
/// полем кадра, отменяет этот ответ, и вызыватель получает `PeerClosed`. Принятый каналом кадр
/// передаёт получателю владельцев без счётчика, и у отправителя закрытие не происходит.
pub struct MockHandle<K: ObjectKind> {
    entry: Entry,
    kind: PhantomData<fn() -> K>,
}

impl<K: ObjectKind> MockHandle<K> {
    /// Владелец с номером `raw`.
    #[must_use]
    pub const fn new(raw: u32) -> Self {
        Self {
            entry: Entry { raw, closes: None },
            kind: PhantomData,
        }
    }

    /// Владелец с номером `raw`, чьё закрытие увеличивает `closes`.
    #[must_use]
    pub fn tracked(raw: u32, closes: &Arc<AtomicUsize>) -> Self {
        Self {
            entry: Entry {
                raw,
                closes: Some(Arc::clone(closes)),
            },
            kind: PhantomData,
        }
    }

    /// Номер владельца.
    #[must_use]
    pub const fn raw(&self) -> u32 {
        self.entry.raw
    }

    fn retype<L: ObjectKind>(self) -> MockHandle<L> {
        MockHandle {
            entry: self.entry,
            kind: PhantomData,
        }
    }

    fn transferred(mut self) -> Self {
        self.entry.closes = None;
        self
    }
}

impl<K: ObjectKind> IntoWireValue<MockResources> for MockHandle<K> {
    type Read = Cap<K>;

    fn write_into(
        self,
        frame: &mut MessageBuf<MockResources>,
        field_id: u8,
    ) -> Result<(), IpcError> {
        frame.write_handle(field_id, self)
    }
}

struct Entry {
    raw: u32,
    closes: Option<Arc<AtomicUsize>>,
}

impl Drop for Entry {
    fn drop(&mut self) {
        if let Some(closes) = &self.closes {
            closes.fetch_add(1, Ordering::SeqCst);
        }
        drop(take_forwarded(self.raw).map(|slot| MockReply { slot: Some(slot) }));
    }
}

impl<K: ObjectKind> PartialEq for MockHandle<K> {
    fn eq(&self, other: &Self) -> bool {
        self.raw() == other.raw()
    }
}

impl<K: ObjectKind> Eq for MockHandle<K> {}

impl<K: ObjectKind> core::fmt::Debug for MockHandle<K> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_tuple("MockHandle").field(&self.raw()).finish()
    }
}

/// Канал ответа одному вызову.
///
/// Разрушение неотправленного канала отменяет ответ, и вызыватель получает `PeerClosed`. Запись
/// канала полем кадра регистрирует его под новым номером capability, начиная с `0x8000_0000`,
/// выше номеров, которые назначают тесты. Получатель кадра читает из этого поля тот же канал.
pub struct MockReply {
    slot: Option<Arc<Slot>>,
}

impl ReplyChannel for MockReply {
    type Resources = MockResources;

    fn send(mut self, frame: Outgoing<'_, MockResources>) -> Result<(), Unsent<'_, MockResources>> {
        let Some(slot) = self.slot.take() else {
            return Err((IpcError::PeerClosed, frame));
        };
        let mut answer = lock(&slot.answer);
        if !matches!(*answer, Answer::Waiting) {
            return Err((IpcError::PeerClosed, frame));
        }
        *answer = Answer::Sent(Frame::new(
            frame.as_bytes().to_vec(),
            frame.into_resources(),
            None,
        ));
        drop(answer);
        slot.settled.notify_all();
        Ok(())
    }
}

impl IntoWireValue<MockResources> for MockReply {
    type Read = Cap<kind::Reply>;

    fn write_into(
        mut self,
        frame: &mut MessageBuf<MockResources>,
        field_id: u8,
    ) -> Result<(), IpcError> {
        let slot = self.slot.take().ok_or(IpcError::PeerClosed)?;
        let id = NEXT_FORWARDED.fetch_add(1, Ordering::Relaxed);
        lock(&FORWARDED).push((id, slot));
        frame.write_handle(field_id, MockHandle::<kind::Reply>::new(id))
    }
}

impl Drop for MockReply {
    fn drop(&mut self) {
        let Some(slot) = self.slot.take() else {
            return;
        };
        let mut answer = lock(&slot.answer);
        if matches!(*answer, Answer::Waiting) {
            *answer = Answer::Refused;
            drop(answer);
            slot.settled.notify_all();
        }
    }
}

impl core::fmt::Debug for MockReply {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("MockReply")
            .field("pending", &self.slot.is_some())
            .finish()
    }
}

/// Клиентская сторона хостового канала.
///
/// Реализация [`AsyncChannel`](ipc::AsyncChannel) выполняет синхронную операцию при создании
/// future, и future готов сразу.
///
/// `send` ставит кадр в очередь и получателя не ждёт. `call` ставит кадр вместе с каналом ответа
/// и ждёт ответа в пределах срока. По истечении срока кадр вызова остаётся в очереди, как кадр,
/// принятый сервером до истечения, и ответ на него получает `PeerClosed`.
pub struct MockChannel {
    port: Arc<Port>,
}

/// Серверная сторона хостового канала.
///
/// `recv` ждёт кадр в пределах срока. Когда все клиентские стороны разрушены, а очередь пуста,
/// `recv` отвечает `Closed`. Разрушение серверной стороны отвергает кадры очереди и дальнейшие
/// отправки с `PeerClosed`.
pub struct MockListener {
    port: Arc<Port>,
}

/// Создаёт связанную пару сторон одного канала.
#[must_use]
pub fn pair() -> (MockChannel, MockListener) {
    let port = Arc::new(Port {
        queue: Mutex::new(Queue {
            frames: VecDeque::new(),
            senders: 1,
            listening: true,
        }),
        readable: Condvar::new(),
    });
    (
        MockChannel {
            port: Arc::clone(&port),
        },
        MockListener { port },
    )
}

impl MockChannel {
    fn enqueue<'b>(
        &self,
        frame: Outgoing<'b, MockResources>,
        reply: Option<Arc<Slot>>,
    ) -> Result<&'b mut FrameBytes, Unsent<'b, MockResources>> {
        let mut queue = lock(&self.port.queue);
        if !queue.listening {
            return Err((IpcError::PeerClosed, frame));
        }
        let bytes = frame.as_bytes().to_vec();
        let OutgoingParts { buffer, resources } = frame.into_parts();
        queue.frames.push_back(Frame::new(bytes, resources, reply));
        drop(queue);
        self.port.readable.notify_all();
        Ok(buffer)
    }
}

impl Clone for MockChannel {
    fn clone(&self) -> Self {
        lock(&self.port.queue).senders += 1;
        Self {
            port: Arc::clone(&self.port),
        }
    }
}

impl Drop for MockChannel {
    fn drop(&mut self) {
        lock(&self.port.queue).senders -= 1;
        self.port.readable.notify_all();
    }
}

impl Channel for MockChannel {
    type Resources = MockResources;

    fn send<'b>(
        &self,
        frame: Outgoing<'b, MockResources>,
        _wait: Timeout,
    ) -> Result<(), Unsent<'b, MockResources>> {
        self.enqueue(frame, None).map(drop)
    }

    fn call<'b>(
        &self,
        frame: Outgoing<'b, MockResources>,
        wait: Timeout,
    ) -> Result<Incoming<'b, MockResources>, IpcError> {
        let slot = Arc::new(Slot {
            answer: Mutex::new(Answer::Waiting),
            settled: Condvar::new(),
        });
        let reply = self
            .enqueue(frame, Some(Arc::clone(&slot)))
            .map_err(|(error, _)| error)?;

        let deadline = Deadline::after(wait);
        let mut answer = lock(&slot.answer);
        while matches!(*answer, Answer::Waiting) {
            let Some(left) = deadline.left() else {
                *answer = Answer::Abandoned;
                return Err(IpcError::Timeout);
            };
            answer = slot
                .settled
                .wait_timeout(answer, left)
                .expect("condvar wait")
                .0;
        }
        match core::mem::replace(&mut *answer, Answer::Abandoned) {
            Answer::Sent(frame) => Ok(frame.deliver(reply)),
            _ => Err(IpcError::PeerClosed),
        }
    }
}

impl MockListener {
    /// Принимает один кадр без срока и передаёт его `handle`.
    ///
    /// # Panics
    ///
    /// Отказ приёма.
    pub fn serve<T>(&self, handle: impl FnOnce(Incoming<'_, MockResources>) -> T) -> T {
        let mut bytes = [0u8; ipc::wire::MESSAGE_INLINE_MAX];
        handle(
            self.recv(&mut bytes, Timeout::INFINITE)
                .expect("a frame arrives"),
        )
    }

    /// Передаёт `handle` кадры по одному, пока канал не закроется либо `handle` не откажет.
    ///
    /// # Errors
    ///
    /// Отказ `handle`.
    ///
    /// # Panics
    ///
    /// Отказ приёма, кроме закрытия канала.
    pub fn serve_all<E>(
        &self,
        mut handle: impl FnMut(Incoming<'_, MockResources>) -> Result<(), E>,
    ) -> Result<(), E> {
        let mut bytes = [0u8; ipc::wire::MESSAGE_INLINE_MAX];
        loop {
            match self.recv(&mut bytes, Timeout::INFINITE) {
                Ok(frame) => handle(frame)?,
                Err(RecvError::Closed) => return Ok(()),
                Err(error) => panic!("receive failed: {error:?}"),
            }
        }
    }
}

impl Drop for MockListener {
    fn drop(&mut self) {
        let mut queue = lock(&self.port.queue);
        queue.listening = false;
        let frames = core::mem::take(&mut queue.frames);
        drop(queue);
        // Каналы ответа кадров очереди разрушаются, и их вызыватели получают `PeerClosed`.
        drop(frames);
    }
}

impl Listener for MockListener {
    type Resources = MockResources;

    fn recv<'b>(
        &self,
        into: &'b mut FrameBytes,
        wait: Timeout,
    ) -> Result<Incoming<'b, MockResources>, RecvError> {
        let deadline = Deadline::after(wait);
        let mut queue = lock(&self.port.queue);
        loop {
            if let Some(frame) = queue.frames.pop_front() {
                drop(queue);
                return Ok(frame.deliver(into));
            }
            if queue.senders == 0 {
                return Err(RecvError::Closed);
            }
            let left = deadline.left().ok_or(RecvError::Timeout)?;
            queue = self
                .port
                .readable
                .wait_timeout(queue, left)
                .expect("condvar wait")
                .0;
        }
    }
}

static FORWARDED: Mutex<Vec<(u32, Arc<Slot>)>> = Mutex::new(Vec::new());

static NEXT_FORWARDED: AtomicU32 = AtomicU32::new(0x8000_0000);

struct Port {
    queue: Mutex<Queue>,
    readable: Condvar,
}

struct Queue {
    frames: VecDeque<Frame>,
    senders: usize,
    listening: bool,
}

struct Slot {
    answer: Mutex<Answer>,
    settled: Condvar,
}

enum Answer {
    Waiting,
    Sent(Frame),
    Refused,
    Abandoned,
}

struct Frame {
    bytes: Vec<u8>,
    handles: Vec<Option<MockHandle<kind::Any>>>,
    reply: Option<MockReply>,
}

impl Frame {
    fn new(bytes: Vec<u8>, resources: MockResources, reply: Option<Arc<Slot>>) -> Self {
        Self {
            bytes,
            handles: resources
                .handles
                .into_iter()
                .map(|handle| handle.map(MockHandle::transferred))
                .collect(),
            reply: reply.map(|slot| MockReply { slot: Some(slot) }),
        }
    }

    fn deliver(self, into: &mut FrameBytes) -> Incoming<'_, MockResources> {
        let len = self.bytes.len();
        into[..len].copy_from_slice(&self.bytes);
        Incoming {
            bytes: &into[..len],
            resources: MockResources {
                handles: self.handles,
                reply: self.reply,
            },
        }
    }
}

struct Deadline(Option<Instant>);

impl Deadline {
    fn after(wait: Timeout) -> Self {
        Self((wait != Timeout::INFINITE).then(|| Instant::now() + Duration::from_nanos(wait.raw())))
    }

    fn left(&self) -> Option<Duration> {
        match self.0 {
            None => Some(Duration::MAX),
            Some(deadline) => deadline
                .checked_duration_since(Instant::now())
                .filter(|left| !left.is_zero()),
        }
    }
}

fn take_forwarded(raw: u32) -> Option<Arc<Slot>> {
    let mut forwarded = lock(&FORWARDED);
    let index = forwarded.iter().position(|(id, _)| *id == raw)?;
    Some(forwarded.swap_remove(index).1)
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().expect("mutex not poisoned")
}

#[cfg(test)]
mod tests {
    use std::thread;

    use ipc::{
        Channel, Listener, ReceivedResources, RecvError, ReplyChannel, Timeout,
        wire::{IpcError, MESSAGE_INLINE_MAX, MessageBuf},
    };
    use syscall::kind;

    use super::{MockHandle, MockResources, pair};

    fn frame(data: &[u8]) -> MessageBuf<MockResources> {
        let mut frame = MessageBuf::new();
        frame.write_field(1, data).expect("field fits");
        frame
    }

    #[test]
    fn a_call_waits_for_the_answer_of_its_reply_channel() {
        let (client, server) = pair();
        thread::scope(|scope| {
            scope.spawn(|| {
                server.serve(|mut incoming| {
                    assert_eq!(incoming.bytes, frame(&[1, 2]).as_bytes());
                    let reply = incoming
                        .resources
                        .take_reply()
                        .expect("a call brings a reply");
                    let mut answer = frame(&[3]);
                    answer
                        .write_handle(2, MockHandle::<kind::Port>::new(7))
                        .expect("handle fits");
                    reply.send(answer.outgoing()).expect("the caller waits");
                });
            });
            let mut sent = frame(&[1, 2]);
            let mut incoming = client
                .call(sent.outgoing(), Timeout::INFINITE)
                .expect("answered");
            assert_eq!(
                incoming.resources.take_handle::<kind::Port>(0),
                Some(MockHandle::new(7))
            );
        });
    }

    #[test]
    fn a_dropped_reply_channel_refuses_its_caller() {
        let (client, server) = pair();
        thread::scope(|scope| {
            scope.spawn(|| server.serve(|incoming| drop(incoming)));
            let mut sent = frame(&[1]);
            assert_eq!(
                client
                    .call(sent.outgoing(), Timeout::INFINITE)
                    .map(|incoming| incoming.bytes.len()),
                Err(IpcError::PeerClosed)
            );
        });
    }

    #[test]
    fn a_one_way_frame_brings_no_reply_channel() {
        let (client, server) = pair();
        client
            .send(frame(&[1]).outgoing(), Timeout::POLL)
            .expect("queued");
        server.serve(|mut incoming| assert!(incoming.resources.take_reply().is_none()));
    }

    #[test]
    fn a_refused_one_way_frame_comes_back_with_its_handles() {
        let (client, server) = pair();
        drop(server);
        let mut sent = frame(&[1]);
        sent.write_handle(2, MockHandle::<kind::Port>::new(5))
            .expect("handle fits");
        let (error, refused) = client
            .send(sent.outgoing(), Timeout::POLL)
            .expect_err("no listener");
        assert_eq!(error, IpcError::PeerClosed);
        let mut resources = refused.into_resources();
        assert_eq!(
            resources.take_handle::<kind::Port>(0),
            Some(MockHandle::new(5))
        );
    }

    #[test]
    fn the_answer_to_a_timed_out_call_is_refused() {
        let (client, server) = pair();
        let mut sent = frame(&[1]);
        assert_eq!(
            client
                .call(sent.outgoing(), Timeout::POLL)
                .map(|incoming| incoming.bytes.len()),
            Err(IpcError::Timeout)
        );
        server.serve(|mut incoming| {
            let reply = incoming
                .resources
                .take_reply()
                .expect("a call brings a reply");
            assert_eq!(
                reply
                    .send(frame(&[2]).outgoing())
                    .map_err(|(error, _)| error),
                Err(IpcError::PeerClosed)
            );
        });
    }

    #[test]
    fn the_listener_closes_once_its_channels_are_gone_and_the_queue_is_drained() {
        let (client, server) = pair();
        client
            .send(frame(&[1]).outgoing(), Timeout::POLL)
            .expect("queued");
        drop(client);
        server.serve(|incoming| assert_eq!(incoming.bytes, frame(&[1]).as_bytes()));
        let mut bytes = [0u8; MESSAGE_INLINE_MAX];
        assert_eq!(
            server
                .recv(&mut bytes, Timeout::INFINITE)
                .map(|incoming| incoming.bytes.len()),
            Err(RecvError::Closed)
        );
    }
}
