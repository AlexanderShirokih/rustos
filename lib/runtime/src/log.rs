//! Строки процесса в системный журнал.
//!
//! Лог-сессия приходит процессу стартовым кадром компонента, и до неё строки уходят в никуда.
//! Источник строк называет сервер журнала по значку сессии.
//!
//! Запись ставит кадр строки отложенной отправкой порта и возвращается сразу после постановки.
//! Строки процесса доходят до журнала в порядке постановки. Поставленных и не принятых кадров
//! не больше [`LOG_PENDING_MAX`]. При заполненной очереди запись ждёт приёма одного из них не
//! дольше `LOG_ROOM_TIMEOUT`. Строка, не получившая места за этот срок, отбрасывается, и
//! следующую поставленную строку предваряет строка с числом отброшенных. [`exit`](crate::exit)
//! ждёт приёма поставленных кадров не дольше [`LOG_FLUSH_TIMEOUT`].

use core::{
    cell::RefCell,
    fmt::Write as _,
    sync::atomic::{AtomicU32, Ordering},
};

use ipc::{Channel, Incoming, Outgoing, Unsent, wire::IpcError};
use log_abi::{LOG_MESSAGE_MAX, LineAssembler, LogBuffer, log};
use syscall::{Handle, SIGNALED, Timeout, WaitItem, kind};

use crate::{
    clock::Instant,
    svc::{Delivery, IssuedHandle, PortTransport, signal_wait_many, take_outcome},
    sync::Mutex,
};

/// Предел кадров журнала, поставленных процессом и ещё не принятых журналом.
pub const LOG_PENDING_MAX: usize = 16;

/// Срок, который [`exit`](crate::exit) ждёт приёма поставленных кадров журнала.
pub const LOG_FLUSH_TIMEOUT: Timeout = Timeout::from_secs(1);

const LOG_ROOM_TIMEOUT: Timeout = Timeout::from_secs(5);

static SESSION: AtomicU32 = AtomicU32::new(0);

static UNCONFIRMED: Mutex<Unconfirmed> = Mutex::new(Unconfirmed::new());

struct Unconfirmed {
    pending: [Option<IssuedHandle<kind::Pending>>; LOG_PENDING_MAX],
    dropped: u64,
}

struct Posting<'q> {
    transport: PortTransport,
    queue: RefCell<&'q mut Unconfirmed>,
    deadline: Option<Instant>,
}

/// Поток вывода, ведущий записи журнала построчно.
///
/// Запись журнала завершается по переводу строки, незавершённую строку отдаёт [`Stream::flush`].
pub struct Stream {
    lines: Mutex<LineAssembler<LOG_MESSAGE_MAX>>,
}

impl Stream {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            lines: Mutex::new(LineAssembler::new()),
        }
    }

    /// Принимает кусок вывода и отдаёт журналу завершённые им строки.
    pub fn write(&self, text: &str) -> Result<(), IpcError> {
        self.emit(|lines, sink| lines.push(text, sink))
    }

    /// Отдаёт журналу незавершённую строку потока.
    pub fn flush(&self) -> Result<(), IpcError> {
        self.emit(|lines, sink| lines.flush(sink))
    }

    fn emit(
        &self,
        take: impl FnOnce(&mut LineAssembler<LOG_MESSAGE_MAX>, &mut dyn FnMut(&str)),
    ) -> Result<(), IpcError> {
        if !installed() {
            return Ok(());
        }
        let mut failure = None;
        take(&mut self.lines.lock(), &mut |line| {
            // Отказ первой записи возвращается вызывающему, остальные строки уходят следом:
            // накопитель уже отдал их, и держать их негде.
            let outcome = post(line);
            failure = failure.take().or_else(|| outcome.err());
        });
        match failure {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }
}

impl Unconfirmed {
    const fn new() -> Self {
        Self {
            pending: [const { None }; LOG_PENDING_MAX],
            dropped: 0,
        }
    }

    fn make_room(&mut self, deadline: Option<Instant>) -> bool {
        loop {
            self.reap();
            if self.pending.iter().any(Option::is_none) {
                return true;
            }

            if !self.wait_outcome(deadline) {
                return false;
            }
        }
    }

    fn wait_outcome(&self, deadline: Option<Instant>) -> bool {
        let mut items = self
            .pending
            .iter()
            .flatten()
            .map(|pending| WaitItem::new(pending.as_raw(), SIGNALED))
            .peekable();
        let Some(&first) = items.peek() else {
            return false;
        };

        // Места сверх числа операций повторяют первую из них, и `SignalWaitMany` ждёт тот же
        // набор.
        let wait: [WaitItem; LOG_PENDING_MAX] =
            core::array::from_fn(|_| items.next().unwrap_or(first));

        let now = Instant::now();
        let left = deadline.map_or(Timeout::INFINITE, |deadline| deadline.since(now));
        if left == Timeout::POLL {
            return false;
        }

        let _ = signal_wait_many(&wait, left);
        true
    }

    fn drop_line(&mut self, error: IpcError) -> Result<(), IpcError> {
        if error != IpcError::Timeout {
            return Err(error);
        }

        self.dropped = self.dropped.saturating_add(1);
        Ok(())
    }

    fn reap(&mut self) {
        for slot in &mut self.pending {
            if let Some(pending) = slot.take() {
                *slot = take_outcome(pending).err();
            }
        }
    }
}

impl Channel for Posting<'_> {
    type Resources = Delivery;

    fn send<'b>(
        &self,
        mut frame: Outgoing<'b, Delivery>,
        _wait: Timeout,
    ) -> Result<(), Unsent<'b, Delivery>> {
        // Успех отправки означает постановку кадра, приёма журналом канал не ждёт.
        let mut queue = self.queue.borrow_mut();
        if !queue.make_room(self.deadline) {
            return Err((IpcError::Timeout, frame));
        }

        let Some(slot) = queue.pending.iter_mut().find(|slot| slot.is_none()) else {
            return Err((IpcError::ResourceExhausted, frame));
        };

        match self.transport.post_send(&mut frame) {
            Ok(pending) => {
                *slot = Some(pending);
                Ok(())
            }
            Err(error) => Err((error, frame)),
        }
    }

    fn call<'b>(
        &self,
        _frame: Outgoing<'b, Delivery>,
        _wait: Timeout,
    ) -> Result<Incoming<'b, Delivery>, IpcError> {
        Err(IpcError::PeerClosed)
    }
}

impl Default for Stream {
    fn default() -> Self {
        Self::new()
    }
}

/// Отвечает, поставлена ли лог-сессия процесса.
#[must_use]
pub fn installed() -> bool {
    SESSION.load(Ordering::Acquire) != 0
}

/// Запоминает лог-сессию процесса.
///
/// Сессию приносит стартовый кадр, и он один. Сессия остаётся открытой до завершения
/// процесса. Повторный вызов закрывает переданную сессию.
pub fn install(session: IssuedHandle<kind::Port>) {
    if SESSION
        .compare_exchange(
            0,
            session.as_raw().raw(),
            Ordering::AcqRel,
            Ordering::Acquire,
        )
        .is_err()
    {
        return;
    }
    let _ = session.leak();
}

/// Пишет текст в журнал построчно, не накапливая незавершённую строку.
///
/// Накопитель этому пути вывода недоступен. Через него идут [`line()`] и [`fmt()`].
pub fn write(text: &str) -> Result<(), IpcError> {
    for line in text.strip_suffix('\n').unwrap_or(text).split('\n') {
        post(line)?;
    }
    Ok(())
}

pub(crate) fn settle() {
    let deadline = Instant::now().after(LOG_FLUSH_TIMEOUT);
    let mut queue = UNCONFIRMED.lock();
    loop {
        queue.reap();
        if !queue.wait_outcome(deadline) {
            return;
        }
    }
}

fn post(line: &str) -> Result<(), IpcError> {
    let Some(session) = crate::restore(SESSION.load(Ordering::Acquire)) else {
        return Ok(());
    };

    let mut queue = UNCONFIRMED.lock();
    let deadline = Instant::now().after(LOG_ROOM_TIMEOUT);

    let dropped = queue.dropped;
    if dropped > 0 {
        let mut notice = LineWriter {
            buffer: LogBuffer::new(),
        };
        let _ = write!(notice, "log: {dropped} lines dropped");
        let posted = posting(session, &mut queue, deadline).write_str(notice.buffer.as_str());
        match posted {
            Ok(()) => queue.dropped = 0,
            Err(error) => return queue.drop_line(error),
        }
    }

    let posted = posting(session, &mut queue, deadline).write_str(line);
    match posted {
        Ok(()) => Ok(()),
        Err(error) => queue.drop_line(error),
    }
}

fn posting(
    session: Handle<kind::Port>,
    queue: &mut Unconfirmed,
    deadline: Option<Instant>,
) -> log::Client<Posting<'_>> {
    log::Client::new(Posting {
        transport: PortTransport::new(session),
        queue: RefCell::new(queue),
        deadline,
    })
}

/// Best-effort строка в журнал. До стартового кадра - no-op.
pub fn line(message: &str) {
    let _ = write(message);
}

/// Форматирует строку на стеке и отправляет её [`line()`]. Строка длиннее кадра уходит
/// несколькими кадрами.
pub fn fmt(args: core::fmt::Arguments<'_>) {
    let mut writer = LineWriter {
        buffer: LogBuffer::new(),
    };
    let _ = writer.write_fmt(args);
    line(writer.buffer.as_str());
}

struct LineWriter {
    buffer: LogBuffer<LOG_MESSAGE_MAX>,
}

impl core::fmt::Write for LineWriter {
    fn write_str(&mut self, text: &str) -> core::fmt::Result {
        let mut rest = text;
        while !rest.is_empty() {
            let taken = self.buffer.push_str(rest);
            rest = &rest[taken..];
            if rest.is_empty() {
                break;
            }
            if taken == 0 && self.buffer.is_empty() {
                // Символ не помещается даже в пустой накопитель, кадр не вмещает его целиком.
                break;
            }
            line(self.buffer.as_str());
            self.buffer.clear();
        }
        Ok(())
    }
}
