//! Запросы `ComponentStartup` по стартовой сессии и заявка импортёра на стороне провайдера.

use component_abi::{AcceptError, Attributes, SessionError, SessionRequirement, component_startup};
use ipc::{CallError, Timeout};
use kernel_objects::{IssuedHandle, Port, PortTransport, Session, kind};
use syscall::Handle;

/// Заявка импортёра на сессию экспорта. Разрушение
/// заявки без ответа даёт импортёру `IpcError::PeerClosed`.
pub type SessionRequest =
    component_abi::SessionRequest<component_startup::session::Reply<IssuedHandle<kind::Reply>>>;

/// Результат поиска. Его сессия родителя принимает `open`.
pub type Found = component_abi::Found<Session<component_startup::Protocol>>;

/// Запрашивает по стартовой сессии `parent` сессию единственного экспорта протокола `P`,
/// совпавшего с `attributes`, по требованию `requirement`, ожидая ответ не дольше `wait`. Срок
/// покрывает `accept` провайдера и его ответ.
///
/// Отказы:
/// - `Ipc(IpcError::Timeout)` - срок истёк, заявка остаётся у родителя до выдачи провайдеру
///   либо до вытеснения сверх предела заявок, повтор ждёт следом за ней;
/// - `Remote` - отказ родителя.
pub fn session<P: ipc::Protocol>(
    parent: Handle<kind::Port>,
    requirement: &str,
    attributes: Attributes<'_>,
    wait: Timeout,
) -> Result<Session<P>, CallError<SessionError>> {
    component_startup::Client::new(PortTransport::new(parent))
        .with_wait(wait)
        .session(query::<P>(requirement), attributes.as_bytes())
        .map(|port| Session::from_handle(Port::from(port)))
}

/// Запрашивает перечисление экспортов протокола `P`, совпавших с `attributes`, по требованию
/// `requirement` вида `all`.
pub fn all<P: ipc::Protocol>(
    parent: &component_startup::Client<PortTransport>,
    requirement: &str,
    attributes: Attributes<'_>,
) -> Result<Session<component_startup::Protocol>, CallError<SessionError>> {
    parent
        .all(query::<P>(requirement), attributes.as_bytes())
        .map(|port| Session::from_handle(Port::from(port)))
}

/// Забирает следующий результат перечисления `enumeration`, ожидая ответ не дольше `wait`.
/// `None` - конец перечисления.
pub fn next(
    enumeration: &Session<component_startup::Protocol>,
    wait: Timeout,
) -> Result<Option<Found>, CallError<SessionError>> {
    let enumeration =
        component_startup::Client::new(PortTransport::new(enumeration.handle())).with_wait(wait);

    let found = enumeration.next()?;
    Ok(found.map(|found| component_abi::Found {
        session: Session::from_handle(Port::from(found.session)),
        protocol: found.protocol,
        filter: found.filter,
    }))
}

/// Открывает сессию экспорта протокола `P` по результату поиска `found`, ожидая ответ не дольше
/// `wait`. Результат другого протокола получает `Remote(SessionError::Refused)` без обращения к
/// родителю.
pub fn open<P: ipc::Protocol>(
    found: &Found,
    wait: Timeout,
) -> Result<Session<P>, CallError<SessionError>> {
    if found.protocol.as_str() != P::ID {
        return Err(CallError::Remote(SessionError::Refused));
    }

    component_startup::Client::new(PortTransport::new(found.session.handle()))
        .with_wait(wait)
        .open()
        .map(|port| Session::from_handle(Port::from(port)))
}

/// Забирает у родителя следующую заявку импортёра на экспорт протокола `P`.
pub fn accept<P: ipc::Protocol>(
    parent: &component_startup::Client<PortTransport>,
) -> Result<SessionRequest, CallError<AcceptError>> {
    parent.accept(P::ID)
}

fn query<P: ipc::Protocol>(requirement: &str) -> SessionRequirement<'_> {
    SessionRequirement {
        name: requirement,
        protocol: P::ID,
    }
}
