//! Обслуживание экспорта провайдером и создание портов сессий.

use core::{
    num::NonZeroU64,
    sync::atomic::{AtomicU64, Ordering},
};

use component_abi::{AcceptError, Attributes, SessionError, component_startup};
use ipc::{CallError, FrameBytes, Incoming, Listener, RecvError, wire::MESSAGE_INLINE_MAX};
use kernel_objects::{Delivery, Error, Port, PortTransport};
use syscall::{Rights, Timeout};

use crate::request::accept;

/// Порт сессии из [`mint_session`] и его значок.
#[derive(Debug)]
pub struct MintedSession {
    pub badge: NonZeroU64,
    pub port: Port,
}

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
/// строит порт сессии по атрибутам запроса импортёра либо отказывает. Возвращает отказ `accept`.
pub fn serve_export<P: ipc::Protocol>(
    parent: &component_startup::Client<PortTransport>,
    mut open: impl FnMut(Attributes<'_>) -> Result<Port, SessionError>,
) -> CallError<AcceptError> {
    loop {
        match accept::<P>(parent) {
            Ok(request) => {
                let session = open(request.attributes.as_attributes());
                let _ = request.reply.send(session);
            }
            Err(error) => return error,
        }
    }
}

/// Обработчик [`serve_export`], выдающий порт `export` под новым значком при любых атрибутах
/// запроса.
pub fn whole_export(
    export: &Port,
) -> impl FnMut(Attributes<'_>) -> Result<Port, SessionError> + '_ {
    let next_badge = AtomicU64::new(0);

    move |_| {
        mint_session(export, &next_badge)
            .map(|minted| minted.port)
            .map_err(|_| SessionError::NoSpace)
    }
}

/// Создаёт клиентскую capability порта `export` с правами `WRITE | TRANSFER` под новым значком
/// из `next_badge`. Значок не повторяется и после отказа.
pub fn mint_session(export: &Port, next_badge: &AtomicU64) -> Result<MintedSession, Error> {
    let badge = NonZeroU64::MIN.saturating_add(next_badge.fetch_add(1, Ordering::Relaxed));
    let port = export.duplicate(Rights::WRITE | Rights::TRANSFER, Some(badge))?;

    Ok(MintedSession { badge, port })
}
