//! Запросы `ComponentStartup` по стартовой сессии и заявка импортёра на стороне провайдера.

use component_abi::{AcceptError, Attributes, SessionError, SessionRequirement, component_startup};
use executor::{PortChannel, Reactor};
use ipc::{CallError, Timeout};
use kernel_objects::{IssuedHandle, Port, PortTransport, Session, kind};
use syscall::Handle;

/// Заявка импортёра на сессию экспорта. Разрушение
/// заявки без ответа даёт импортёру `IpcError::PeerClosed`.
pub type SessionRequest =
    component_abi::SessionRequest<component_startup::session::Reply<IssuedHandle<kind::Reply>>>;

/// Результат поиска. Его сессия родителя принимает `open`.
pub type Found = component_abi::Found<Session<component_startup::Protocol>>;

/// Клиент стартовой сессии для задач `executor`. Ответ родителя задача ждёт через reactor
/// канала.
pub type AsyncStartup = component_startup::AsyncClient<PortChannel>;

type ReceivedFound = component_abi::Found<IssuedHandle<kind::Port>>;

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
        .map(owned_session)
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
        .map(owned_session)
}

/// Запрашивает наблюдение за экспортами протокола `P`, совпавшими с `attributes`, по требованию
/// `requirement` вида `all`. Результаты наблюдения выдаёт [`next`].
pub fn watch<P: ipc::Protocol>(
    parent: &component_startup::Client<PortTransport>,
    requirement: &str,
    attributes: Attributes<'_>,
) -> Result<Session<component_startup::Protocol>, CallError<SessionError>> {
    parent
        .watch(query::<P>(requirement), attributes.as_bytes())
        .map(owned_session)
}

/// Забирает следующий результат перечисления либо наблюдения `enumeration`, ожидая ответ не
/// дольше `wait`. `None` - конец перечисления.
pub fn next(
    enumeration: &Session<component_startup::Protocol>,
    wait: Timeout,
) -> Result<Option<Found>, CallError<SessionError>> {
    let enumeration =
        component_startup::Client::new(PortTransport::new(enumeration.handle())).with_wait(wait);

    Ok(enumeration.next()?.map(owned_found))
}

pub(crate) async fn next_async(
    enumeration: &Session<component_startup::Protocol>,
    reactor: &Reactor,
    wait: Timeout,
) -> Result<Option<Found>, CallError<SessionError>> {
    let enumeration =
        AsyncStartup::new(PortChannel::new(enumeration.handle(), reactor.clone())).with_wait(wait);

    Ok(enumeration.next().await?.map(owned_found))
}

/// Открывает сессию экспорта протокола `P` по результату поиска `found`, ожидая ответ не дольше
/// `wait`. Результат другого протокола получает `Remote(SessionError::Refused)` без обращения к
/// родителю.
pub fn open<P: ipc::Protocol>(
    found: &Found,
    wait: Timeout,
) -> Result<Session<P>, CallError<SessionError>> {
    refuse_foreign::<P>(found)?;

    component_startup::Client::new(PortTransport::new(found.session.handle()))
        .with_wait(wait)
        .open()
        .map(owned_session)
}

pub(crate) async fn open_async<P: ipc::Protocol>(
    found: &Found,
    reactor: &Reactor,
    wait: Timeout,
) -> Result<Session<P>, CallError<SessionError>> {
    refuse_foreign::<P>(found)?;

    AsyncStartup::new(PortChannel::new(found.session.handle(), reactor.clone()))
        .with_wait(wait)
        .open()
        .await
        .map(owned_session)
}

/// Забирает у родителя следующую заявку импортёра на экспорт протокола `P`.
pub fn accept<P: ipc::Protocol>(
    parent: &component_startup::Client<PortTransport>,
) -> Result<SessionRequest, CallError<AcceptError>> {
    parent.accept(P::ID)
}

/// Забирает у родителя следующую заявку импортёра на экспорт протокола `P`, как [`accept`].
/// Ответ задача ждёт через reactor клиента `parent`.
pub async fn accept_async<P: ipc::Protocol>(
    parent: &AsyncStartup,
) -> Result<SessionRequest, CallError<AcceptError>> {
    parent.accept(P::ID).await
}

fn refuse_foreign<P: ipc::Protocol>(found: &Found) -> Result<(), CallError<SessionError>> {
    if found.protocol.as_str() == P::ID {
        Ok(())
    } else {
        Err(CallError::Remote(SessionError::Refused))
    }
}

fn owned_session<P: ipc::Protocol>(port: IssuedHandle<kind::Port>) -> Session<P> {
    Session::from_handle(Port::from(port))
}

fn owned_found(found: ReceivedFound) -> Found {
    component_abi::Found {
        session: owned_session(found.session),
        protocol: found.protocol,
        filter: found.filter,
    }
}

fn query<P: ipc::Protocol>(requirement: &str) -> SessionRequirement<'_> {
    SessionRequirement {
        name: requirement,
        protocol: P::ID,
    }
}
