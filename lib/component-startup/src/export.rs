//! Обслуживание экспорта провайдером и создание портов сессий.

use core::{
    borrow::Borrow,
    fmt,
    num::NonZeroU64,
    sync::atomic::{AtomicU64, Ordering},
};

use component_abi::{AcceptError, Attributes, SessionError};
use ipc::{CallError, FrameBytes, Incoming, Listener, RecvError, wire::MESSAGE_INLINE_MAX};
use kernel_objects::{Delivery, Error, Port, PortTransport};
use syscall::{READABLE, Rights, Timeout, WaitItem};

use crate::request::{AsyncStartup, accept};

/// Порт сессий экспорта и счётчик его значков. Значок не повторяется и после отказа.
#[derive(Debug)]
pub struct SessionPort {
    port: Port,
    next_badge: AtomicU64,
}

/// Порт сессии из [`SessionPort::create_session`] и его значок.
#[derive(Debug)]
pub struct MintedSession {
    pub badge: NonZeroU64,
    pub port: Port,
}

/// Отказ `accept`, которым остановился [`serve_export`].
#[derive(Debug)]
pub struct ExportStopped(CallError<AcceptError>);

/// Событие порта сессий экспорта из [`SessionEvents`].
pub enum SessionEvent<'b> {
    /// Кадр сессии со значком отправителя. Разрушение кадра без `dispatch` отменяет ответ, и
    /// вызыватель получает отказ.
    Frame(Incoming<'b, Delivery>),
    /// Ушли последние отправители значка.
    Gone(u64),
}

/// Асинхронный приём порта сессий экспорта в собственный буфер кадра.
///
/// Кадр, отброшенный ядром из-за нехватки места под его capability, пропускается.
pub struct SessionEvents<'p> {
    port: &'p Port,
    bytes: FrameBytes,
}

impl SessionPort {
    /// Создаёт порт сессий экспорта.
    pub fn create() -> Result<Self, Error> {
        Ok(Self {
            port: Port::create()?,
            next_badge: AtomicU64::new(0),
        })
    }

    /// Серверная сторона порта.
    #[must_use]
    pub const fn port(&self) -> &Port {
        &self.port
    }

    /// Создаёт новый значок порта без capability.
    pub fn create_badge(&self) -> NonZeroU64 {
        NonZeroU64::MIN.saturating_add(self.next_badge.fetch_add(1, Ordering::Relaxed))
    }

    /// Создаёт клиентскую capability порта с правами `DUPLICATE | WRITE | TRANSFER` под новым
    /// значком. Закрытие клиентской capability отзывает её производные.
    pub fn create_session(&self) -> Result<MintedSession, Error> {
        let badge = self.create_badge();
        let port = self.port.duplicate(
            Rights::DUPLICATE | Rights::WRITE | Rights::TRANSFER,
            Some(badge),
        )?;

        Ok(MintedSession { badge, port })
    }

    /// Выдаёт порт сессии экспорта под новым значком в ответ на заявку импортёра.
    pub fn open_session(&self) -> Result<Port, SessionError> {
        self.create_session()
            .map(|minted| minted.port)
            .map_err(|_| SessionError::NoSpace)
    }

    /// Приём кадров порта, как [`SessionEvents::new`].
    #[must_use]
    pub const fn events(&self) -> SessionEvents<'_> {
        SessionEvents::new(&self.port)
    }
}

impl fmt::Display for ExportStopped {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "export {:?}", self.0)
    }
}

impl<'p> SessionEvents<'p> {
    #[must_use]
    pub const fn new(port: &'p Port) -> Self {
        Self {
            port,
            bytes: [0; MESSAGE_INLINE_MAX],
        }
    }

    /// Ждёт следующий кадр сессии либо уход значка, уступая поток executor.
    /// `None` означает конец приёма, ошибка - отказ ожидания порта.
    pub async fn recv(&mut self) -> Result<Option<SessionEvent<'_>>, Error> {
        let transport = PortTransport::new(self.port.borrow());
        let (len, resources) = loop {
            match transport.recv(&mut self.bytes, Timeout::POLL) {
                Ok(frame) => break (frame.bytes.len(), frame.resources),
                Err(RecvError::SenderGone(badge)) => return Ok(Some(SessionEvent::Gone(badge))),
                Err(RecvError::Timeout) => {
                    executor::wait(WaitItem::new(self.port.as_raw(), READABLE)).await?;
                }
                Err(RecvError::Discarded) => {}
                Err(RecvError::Closed) => return Ok(None),
            }
        };

        Ok(Some(self.frame(len, resources)))
    }

    fn frame(&self, len: usize, resources: Delivery) -> SessionEvent<'_> {
        SessionEvent::Frame(Incoming {
            bytes: self.bytes.get(..len).unwrap_or_default(),
            resources,
        })
    }
}

/// Отвечает на заявки импортёров на экспорт протокола `P` до первого отказа `accept`.
/// Сессию строит асинхронный `open`.
pub async fn serve_export<P: ipc::Protocol>(
    parent: AsyncStartup,
    mut open: impl AsyncFnMut(Attributes<'_>) -> Result<Port, SessionError>,
) -> ExportStopped {
    let parent = parent.with_wait(Timeout::INFINITE);
    loop {
        match accept::<P>(&parent).await {
            Ok(request) => {
                let session = open(request.attributes.as_attributes()).await;
                let _ = request.reply.send(session);
            }

            Err(error) => return ExportStopped(error),
        }
    }
}

/// Выдаёт сессию `export` из [`SessionPort::open_session`] при любых атрибутах запроса
/// из [`serve_export`].
pub fn whole_export(
    export: impl Borrow<SessionPort>,
) -> impl AsyncFnMut(Attributes<'_>) -> Result<Port, SessionError> {
    async move |_| export.borrow().open_session()
}
