//! Запросы `session` и `accept` по стартовой сессии и заявка импортёра на стороне провайдера.

use component_abi::{AcceptError, SessionError, SessionRequirement, component_startup};
use ipc::wire::{IpcError, Str};
use kernel_objects::{IssuedHandle, Port, PortTransport, Session, kind};

/// Заявка импортёра на сессию экспорта. Разрушение
/// заявки без ответа даёт импортёру `IpcError::PeerClosed`.
pub type SessionRequest =
    component_abi::SessionRequest<component_startup::session::Reply<IssuedHandle<kind::Reply>>>;

/// Запрашивает сессию провайдера `index` требования `requirement` протокола `P`. Внешний
/// `Err` - отказ запроса, внутренний - отказ родителя. Имя длиннее
/// [`REQUIREMENT_NAME_MAX`](component_abi::REQUIREMENT_NAME_MAX) даёт `IpcError::InvalidValue`.
pub fn session<P: ipc::Protocol>(
    parent: &component_startup::Client<PortTransport>,
    requirement: &str,
    index: u16,
) -> Result<Result<Session<P>, SessionError>, IpcError> {
    let requirement = SessionRequirement {
        name: Str::new(requirement).ok_or(IpcError::InvalidValue)?,
        protocol: Str::new(P::ID).ok_or(IpcError::InvalidValue)?,
        index,
    };

    Ok(parent
        .session(requirement)?
        .map(|port| Session::from_handle(Port::from(port))))
}

/// Забирает у родителя следующую заявку импортёра на экспорт протокола `P`. Внешний `Err` -
/// отказ запроса, внутренний - отказ родителя.
pub fn accept<P: ipc::Protocol>(
    parent: &component_startup::Client<PortTransport>,
) -> Result<Result<SessionRequest, AcceptError>, IpcError> {
    parent.accept(Str::new(P::ID).ok_or(IpcError::InvalidValue)?)
}
