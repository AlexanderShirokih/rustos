//! Запросы `ComponentStartup` по стартовой сессии и заявка импортёра на стороне провайдера.

use component_abi::{AcceptError, Attributes, SessionError, SessionRequirement, component_startup};
use ipc::{
    Timeout,
    wire::{Bytes, IpcError, Str},
};
use kernel_objects::{IssuedHandle, Port, PortTransport, Session, kind};

/// Заявка импортёра на сессию экспорта. Разрушение
/// заявки без ответа даёт импортёру `IpcError::PeerClosed`.
pub type SessionRequest =
    component_abi::SessionRequest<component_startup::session::Reply<IssuedHandle<kind::Reply>>>;

/// Результат поиска. Его сессия родителя принимает `open`.
pub type Found = component_abi::Found<Session<component_startup::Protocol>>;

/// Запрашивает сессию единственного экспорта протокола `P`, совпавшего с `attributes`, по
/// требованию `requirement`. Внешний `Err` - отказ запроса, внутренний - отказ родителя. Имя
/// длиннее [`REQUIREMENT_NAME_MAX`](component_abi::REQUIREMENT_NAME_MAX) даёт
/// `IpcError::InvalidValue`.
pub fn session<P: ipc::Protocol>(
    parent: &component_startup::Client<PortTransport>,
    requirement: &str,
    attributes: Attributes<'_>,
) -> Result<Result<Session<P>, SessionError>, IpcError> {
    Ok(parent
        .session(query::<P>(requirement)?, wire(attributes)?)?
        .map(|port| Session::from_handle(Port::from(port))))
}

/// Запрашивает перечисление экспортов протокола `P`, совпавших с `attributes`, по требованию
/// `requirement` вида `all`.
pub fn all<P: ipc::Protocol>(
    parent: &component_startup::Client<PortTransport>,
    requirement: &str,
    attributes: Attributes<'_>,
) -> Result<Result<Session<component_startup::Protocol>, SessionError>, IpcError> {
    Ok(parent
        .all(query::<P>(requirement)?, wire(attributes)?)?
        .map(|port| Session::from_handle(Port::from(port))))
}

/// Забирает следующий результат перечисления `enumeration`, ожидая ответ не дольше `wait`.
/// `None` - конец перечисления.
pub fn next(
    enumeration: &Session<component_startup::Protocol>,
    wait: Timeout,
) -> Result<Result<Option<Found>, SessionError>, IpcError> {
    let enumeration =
        component_startup::Client::new(PortTransport::new(enumeration.handle())).with_wait(wait);

    Ok(enumeration.next()?.map(|found| {
        found.map(|found| component_abi::Found {
            session: Session::from_handle(Port::from(found.session)),
            protocol: found.protocol,
            filter: found.filter,
        })
    }))
}

/// Открывает сессию экспорта протокола `P` по результату поиска `found`, ожидая ответ не дольше
/// `wait`. Результат другого протокола получает `SessionError::Refused` без обращения к родителю.
pub fn open<P: ipc::Protocol>(
    found: &Found,
    wait: Timeout,
) -> Result<Result<Session<P>, SessionError>, IpcError> {
    if found.protocol.as_str() != P::ID {
        return Ok(Err(SessionError::Refused));
    }

    Ok(
        component_startup::Client::new(PortTransport::new(found.session.handle()))
            .with_wait(wait)
            .open()?
            .map(|port| Session::from_handle(Port::from(port))),
    )
}

/// Забирает у родителя следующую заявку импортёра на экспорт протокола `P`. Внешний `Err` -
/// отказ запроса, внутренний - отказ родителя.
pub fn accept<P: ipc::Protocol>(
    parent: &component_startup::Client<PortTransport>,
) -> Result<Result<SessionRequest, AcceptError>, IpcError> {
    parent.accept(Str::new(P::ID).ok_or(IpcError::InvalidValue)?)
}

fn query<P: ipc::Protocol>(requirement: &str) -> Result<SessionRequirement<'_>, IpcError> {
    Ok(SessionRequirement {
        name: Str::new(requirement).ok_or(IpcError::InvalidValue)?,
        protocol: Str::new(P::ID).ok_or(IpcError::InvalidValue)?,
    })
}

fn wire(attributes: Attributes<'_>) -> Result<component_abi::QueryAttributes<'_>, IpcError> {
    Bytes::new(attributes.as_bytes()).ok_or(IpcError::InvalidValue)
}
