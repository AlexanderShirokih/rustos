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
    sync::{
        Arc, Condvar, Mutex, MutexGuard,
        atomic::{AtomicU32, Ordering},
    },
    time::{Duration, Instant},
};

use ipc::{
    Channel, FrameBytes, Incoming, IntoWireValue, Listener, ReceivedResources, RecvError,
    ReplyChannel, Timeout,
    wire::{Cap, IpcError, MessageBuf},
};
use syscall::{Handle, ObjectKind, kind};

/// Capability принятого кадра как символические значения по слотам. [`Cap`] владения не
/// держит, и невостребованное значение мок просто отбрасывает. Вид результата задаёт вид поля
/// контракта, а фактический объект ядра мок не проверяет.
#[derive(Debug, Default)]
pub struct MockResources {
    handles: Vec<Option<u32>>,
    reply: Option<MockReply>,
}

impl ReceivedResources for MockResources {
    type Handle<K: ObjectKind> = Cap<K>;
    type Reply = MockReply;

    fn take_handle<K: ObjectKind>(&mut self, index: u8) -> Option<Cap<K>> {
        let raw = self.handles.get_mut(usize::from(index))?.take()?;
        let id = core::num::NonZeroU32::new(raw)?;
        Some(Cap::from_handle(Handle::from_abi(id)))
    }

    fn reply_channel(handle: Cap<kind::Reply>) -> Option<MockReply> {
        let mut forwarded = lock(&FORWARDED);
        let index = forwarded
            .iter()
            .position(|(id, _)| *id == handle.raw().get())?;
        let (_, slot) = forwarded.swap_remove(index);
        Some(MockReply { slot: Some(slot) })
    }

    fn take_reply(&mut self) -> Option<MockReply> {
        self.reply.take()
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

impl MockReply {
    fn settle(&mut self, outcome: Answer) -> Result<(), IpcError> {
        let slot = self.slot.take().ok_or(IpcError::PeerClosed)?;
        let mut answer = lock(&slot.answer);
        if !matches!(*answer, Answer::Waiting) {
            return Err(IpcError::PeerClosed);
        }
        *answer = outcome;
        drop(answer);
        slot.settled.notify_all();
        Ok(())
    }
}

impl ReplyChannel for MockReply {
    fn send(mut self, bytes: &[u8], handles: &[u32]) -> Result<(), IpcError> {
        self.settle(Answer::Sent(Frame::new(bytes, handles, None)))
    }
}

impl IntoWireValue for MockReply {
    type Read = Cap<kind::Reply>;

    fn write_as_field(&self, buf: &mut MessageBuf, field_id: u8) -> Result<(), IpcError> {
        let slot = self.slot.as_ref().ok_or(IpcError::PeerClosed)?;
        let id = NEXT_FORWARDED.fetch_add(1, Ordering::Relaxed);
        lock(&FORWARDED).push((id, Arc::clone(slot)));
        buf.write_handle(field_id, id)
    }

    fn relinquish(mut self) {
        self.slot = None;
    }
}

impl Drop for MockReply {
    fn drop(&mut self) {
        let _ = self.settle(Answer::Refused);
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
    fn enqueue(&self, frame: Frame) -> Result<(), IpcError> {
        let mut queue = lock(&self.port.queue);
        if !queue.listening {
            return Err(IpcError::PeerClosed);
        }
        queue.frames.push_back(frame);
        drop(queue);
        self.port.readable.notify_all();
        Ok(())
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

    fn send(&self, bytes: &[u8], handles: &[u32], _wait: Timeout) -> Result<(), IpcError> {
        self.enqueue(Frame::new(bytes, handles, None))
    }

    fn call<'b>(
        &self,
        bytes: &[u8],
        handles: &[u32],
        reply: &'b mut FrameBytes,
        wait: Timeout,
    ) -> Result<Incoming<'b, MockResources>, IpcError> {
        let slot = Arc::new(Slot {
            answer: Mutex::new(Answer::Waiting),
            settled: Condvar::new(),
        });
        self.enqueue(Frame::new(bytes, handles, Some(Arc::clone(&slot))))?;

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
    handles: Vec<u32>,
    reply: Option<MockReply>,
}

impl Frame {
    fn new(bytes: &[u8], handles: &[u32], reply: Option<Arc<Slot>>) -> Self {
        Self {
            bytes: bytes.to_vec(),
            handles: handles.to_vec(),
            reply: reply.map(|slot| MockReply { slot: Some(slot) }),
        }
    }

    fn deliver(self, into: &mut FrameBytes) -> Incoming<'_, MockResources> {
        let len = self.bytes.len();
        into[..len].copy_from_slice(&self.bytes);
        Incoming {
            bytes: &into[..len],
            resources: MockResources {
                handles: self.handles.into_iter().map(Some).collect(),
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

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().expect("mutex not poisoned")
}

#[cfg(test)]
mod tests {
    use std::thread;

    use ipc::{
        Channel, Listener, ReceivedResources, RecvError, ReplyChannel, Timeout,
        wire::{Cap, IpcError, MESSAGE_INLINE_MAX},
    };
    use syscall::{Handle, kind};

    use super::pair;

    #[test]
    fn a_call_waits_for_the_answer_of_its_reply_channel() {
        let (client, server) = pair();
        thread::scope(|scope| {
            scope.spawn(|| {
                server.serve(|mut frame| {
                    assert_eq!(frame.bytes, [1, 2]);
                    let reply = frame.resources.take_reply().expect("a call brings a reply");
                    reply.send(&[3], &[7]).expect("the caller waits");
                });
            });
            let mut answer = [0u8; MESSAGE_INLINE_MAX];
            let mut frame = client
                .call(&[1, 2], &[], &mut answer, Timeout::INFINITE)
                .expect("answered");
            assert_eq!(frame.bytes, [3]);
            assert_eq!(
                frame.resources.take_handle::<kind::Port>(0),
                Some(Cap::from_handle(Handle::from_abi(
                    core::num::NonZeroU32::new(7).expect("non-zero")
                )))
            );
        });
    }

    #[test]
    fn a_dropped_reply_channel_refuses_its_caller() {
        let (client, server) = pair();
        thread::scope(|scope| {
            scope.spawn(|| server.serve(|frame| drop(frame)));
            let mut answer = [0u8; MESSAGE_INLINE_MAX];
            assert_eq!(
                client
                    .call(&[1], &[], &mut answer, Timeout::INFINITE)
                    .map(|frame| frame.bytes.len()),
                Err(IpcError::PeerClosed)
            );
        });
    }

    #[test]
    fn a_one_way_frame_brings_no_reply_channel() {
        let (client, server) = pair();
        client.send(&[1], &[], Timeout::POLL).expect("queued");
        server.serve(|mut frame| assert!(frame.resources.take_reply().is_none()));
    }

    #[test]
    fn the_answer_to_a_timed_out_call_is_refused() {
        let (client, server) = pair();
        let mut answer = [0u8; MESSAGE_INLINE_MAX];
        assert_eq!(
            client
                .call(&[1], &[], &mut answer, Timeout::POLL)
                .map(|frame| frame.bytes.len()),
            Err(IpcError::Timeout)
        );
        server.serve(|mut frame| {
            let reply = frame.resources.take_reply().expect("a call brings a reply");
            assert_eq!(reply.send(&[2], &[]), Err(IpcError::PeerClosed));
        });
    }

    #[test]
    fn the_listener_closes_once_its_channels_are_gone_and_the_queue_is_drained() {
        let (client, server) = pair();
        client.send(&[1], &[], Timeout::POLL).expect("queued");
        drop(client);
        server.serve(|frame| assert_eq!(frame.bytes, [1]));
        let mut bytes = [0u8; MESSAGE_INLINE_MAX];
        assert_eq!(
            server
                .recv(&mut bytes, Timeout::INFINITE)
                .map(|frame| frame.bytes.len()),
            Err(RecvError::Closed)
        );
    }
}
