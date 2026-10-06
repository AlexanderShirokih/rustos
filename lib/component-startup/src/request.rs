//! Запросы `ComponentStartup` по стартовой сессии и заявка импортёра на стороне провайдера.

use component_abi::{AcceptError, Attributes, SessionError, SessionRequirement, component_startup};
use ipc::{
    Timeout,
    wire::{Bytes, IpcError, Str},
};
use kernel_objects::{IssuedHandle, Port, PortTransport, Session, kind};
use syscall::Handle;

/// Заявка импортёра на сессию экспорта. Разрушение
/// заявки без ответа даёт импортёру `IpcError::PeerClosed`.
pub type SessionRequest =
    component_abi::SessionRequest<component_startup::session::Reply<IssuedHandle<kind::Reply>>>;

/// Результат поиска. Его сессия родителя принимает `open`.
pub type Found = component_abi::Found<Session<component_startup::Protocol>>;

/// Отказ запроса по стартовой сессии.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequestError<E = SessionError> {
    /// Отказ родителя.
    Parent(E),
    /// Отказ транспорта либо истечение срока ответа.
    Ipc(IpcError),
}

impl<E> From<IpcError> for RequestError<E> {
    fn from(error: IpcError) -> Self {
        Self::Ipc(error)
    }
}

/// Запрашивает по стартовой сессии `parent` сессию единственного экспорта протокола `P`,
/// совпавшего с `attributes`, по требованию `requirement`, ожидая ответ не дольше `wait`. Срок
/// покрывает ожидание публикации признака, `accept` провайдера и его ответ.
///
/// Отказы:
/// - `Ipc(IpcError::InvalidValue)` - имя длиннее
///   [`REQUIREMENT_NAME_MAX`](component_abi::REQUIREMENT_NAME_MAX);
/// - `Ipc(IpcError::Timeout)` - срок истёк, заявка остаётся у родителя до повтора требования;
/// - `Parent` - отказ родителя.
pub fn session<P: ipc::Protocol>(
    parent: Handle<kind::Port>,
    requirement: &str,
    attributes: Attributes<'_>,
    wait: Timeout,
) -> Result<Session<P>, RequestError> {
    component_startup::Client::new(PortTransport::new(parent))
        .with_wait(wait)
        .session(query::<P>(requirement)?, wire(attributes)?)?
        .map(|port| Session::from_handle(Port::from(port)))
        .map_err(RequestError::Parent)
}

/// Запрашивает перечисление экспортов протокола `P`, совпавших с `attributes`, по требованию
/// `requirement` вида `all`.
pub fn all<P: ipc::Protocol>(
    parent: &component_startup::Client<PortTransport>,
    requirement: &str,
    attributes: Attributes<'_>,
) -> Result<Session<component_startup::Protocol>, RequestError> {
    parent
        .all(query::<P>(requirement)?, wire(attributes)?)?
        .map(|port| Session::from_handle(Port::from(port)))
        .map_err(RequestError::Parent)
}

/// Забирает следующий результат перечисления `enumeration`, ожидая ответ не дольше `wait`.
/// `None` - конец перечисления.
pub fn next(
    enumeration: &Session<component_startup::Protocol>,
    wait: Timeout,
) -> Result<Option<Found>, RequestError> {
    let enumeration =
        component_startup::Client::new(PortTransport::new(enumeration.handle())).with_wait(wait);

    let found = enumeration.next()?.map_err(RequestError::Parent)?;
    Ok(found.map(|found| component_abi::Found {
        session: Session::from_handle(Port::from(found.session)),
        protocol: found.protocol,
        filter: found.filter,
    }))
}

/// Открывает сессию экспорта протокола `P` по результату поиска `found`, ожидая ответ не дольше
/// `wait`. Результат другого протокола получает `Parent(SessionError::Refused)` без обращения к
/// родителю.
pub fn open<P: ipc::Protocol>(found: &Found, wait: Timeout) -> Result<Session<P>, RequestError> {
    if found.protocol.as_str() != P::ID {
        return Err(RequestError::Parent(SessionError::Refused));
    }

    component_startup::Client::new(PortTransport::new(found.session.handle()))
        .with_wait(wait)
        .open()?
        .map(|port| Session::from_handle(Port::from(port)))
        .map_err(RequestError::Parent)
}

/// Забирает у родителя следующую заявку импортёра на экспорт протокола `P`.
pub fn accept<P: ipc::Protocol>(
    parent: &component_startup::Client<PortTransport>,
) -> Result<SessionRequest, RequestError<AcceptError>> {
    parent
        .accept(Str::new(P::ID).ok_or(IpcError::InvalidValue)?)?
        .map_err(RequestError::Parent)
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
