//! Обслуживание экспорта провайдером и создание портов сессий.

use core::{
    fmt,
    num::NonZeroU64,
    sync::atomic::{AtomicU64, Ordering},
};

use component_abi::{AcceptError, Attributes, SessionError, component_startup};
use ipc::{CallError, FrameBytes, Incoming, Listener, RecvError, wire::MESSAGE_INLINE_MAX};
use kernel_objects::{Delivery, Error, Port, PortTransport};
use syscall::{Rights, Timeout};

use crate::request::accept;

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

/// Остановка [`serve_export`]: отказ `accept`.
#[derive(Debug)]
pub struct ExportStopped(CallError<AcceptError>);

/// Событие порта сессий экспорта из [`SessionEvents::recv`].
pub enum SessionEvent<'b> {
    /// Кадр сессии со значком отправителя. Разрушение кадра без `dispatch` отменяет ответ, и
    /// вызыватель получает отказ.
    Frame(Incoming<'b, Delivery>),
    /// Ушли последние отправители значка.
    Gone(u64),
}

/// Бессрочный приём порта сессий экспорта в собственный буфер кадра.
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

    /// Создаёт клиентскую capability порта с правами `WRITE | TRANSFER` под новым значком.
    pub fn create_session(&self) -> Result<MintedSession, Error> {
        let badge = self.create_badge();
        let port = self
            .port
            .duplicate(Rights::WRITE | Rights::TRANSFER, Some(badge))?;

        Ok(MintedSession { badge, port })
    }

    /// Бессрочный приём кадров порта, как [`SessionEvents::new`].
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

    /// Возвращает следующий кадр сессии либо уход значка. `None` означает, что кадров порта
    /// больше не будет.
    pub fn recv(&mut self) -> Option<SessionEvent<'_>> {
        let transport = PortTransport::new(self.port.borrow());

        // Из цикла выходят длина и ресурсы кадра, поэтому заём буфера не переходит на следующий
        // приём.
        let (len, resources) = loop {
            match transport.recv(&mut self.bytes, Timeout::INFINITE) {
                Ok(frame) => break (frame.bytes.len(), frame.resources),
                Err(RecvError::SenderGone(badge)) => return Some(SessionEvent::Gone(badge)),
                Err(RecvError::Discarded | RecvError::Timeout) => {}
                Err(RecvError::Closed) => return None,
            }
        };

        Some(SessionEvent::Frame(Incoming {
            bytes: self.bytes.get(..len).unwrap_or_default(),
            resources,
        }))
    }
}

/// Отвечает на заявки импортёров на экспорт протокола `P` до первого отказа `accept`. `open`
/// строит порт сессии по атрибутам запроса импортёра либо отказывает. Цикл ждёт заявку
/// бессрочно.
pub fn serve_export<P: ipc::Protocol>(
    parent: component_startup::Client<PortTransport>,
    mut open: impl FnMut(Attributes<'_>) -> Result<Port, SessionError>,
) -> ExportStopped {
    let parent = parent.with_wait(Timeout::INFINITE);
    loop {
        match accept::<P>(&parent) {
            Ok(request) => {
                let session = open(request.attributes.as_attributes());
                let _ = request.reply.send(session);
            }
            Err(error) => return ExportStopped(error),
        }
    }
}

/// Обработчик [`serve_export`], выдающий сессию `export` под новым значком при любых атрибутах
/// запроса.
pub fn whole_export(
    export: &SessionPort,
) -> impl FnMut(Attributes<'_>) -> Result<Port, SessionError> + '_ {
    move |_| {
        export
            .create_session()
            .map(|minted| minted.port)
            .map_err(|_| SessionError::NoSpace)
    }
}
