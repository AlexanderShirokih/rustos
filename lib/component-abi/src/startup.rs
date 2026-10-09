//! Протокол [`component_startup`]. Компонент запрашивает им у родителя ресурсы по имени
//! требования, ищет экспорты провайдеров, а провайдер принимает им заявки импортёров на свой
//! экспорт.
//!
//! Код выхода 0 сообщает родителю, что компонент закончил работу, ненулевой код - об отказе.

use core::fmt;

use ipc::{
    IntoWireValue, ReceivedResources, WireTyped, WireValue,
    schema::{WireType, max_body_len},
    wire::{BODY_MAX, IpcError, MessageBuf, value},
};
use syscall::kind;

use crate::attributes::{ATTRIBUTES_MAX_BYTES, AttributeSet};

/// Граница длины имени требования в байтах.
pub const REQUIREMENT_NAME_MAX: usize = 64;

/// Срок одного запроса компонента к родителю по стартовой сессии.
pub const STARTUP_REQUEST_TIMEOUT: ipc::Timeout = ipc::Timeout::from_secs(5);

/// Граница длины идентификатора протокола [`ipc::Protocol::ID`] в байтах.
pub const PROTOCOL_ID_MAX: usize = 64;

/// Имя, под которым компонент запрашивает сессию журнала с протоколом `Log`. Манифест это имя
/// не объявляет. Родитель без журнала отвечает на запрос `Refused`.
pub const LOG_REQUIREMENT: &str = "log";

/// Байты атрибутов запроса в кодировке [`Attributes`](crate::Attributes). Родитель разбирает
/// их сам и отвечает на недопустимые `Refused`.
pub type QueryAttributes<'a> = &'a [u8];

ipc::wire_enum! {
    /// Отказ в сессии требования. `Unbound`, `Refused` и `Unavailable` окончательны.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum SessionError {
        /// Заявку вытеснили сверх предела либо не хватило ресурса.
        NoSpace = 1,
        /// Имя, вид, протокол или атрибуты недопустимы, совпало несколько экспортов либо
        /// операция не допускается по этой сессии.
        Refused = 2,
        /// Провайдер экспорта снят навсегда.
        Unavailable = 3,
        /// Ни один экспорт не совпал с запросом.
        Unbound = 4,
    }
}

ipc::wire_enum! {
    /// Отказ в заявке на экспорт.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum AcceptError {
        /// Повтор `accept` тем же провайдером заменил этот запрос.
        NoSpace = 1,
        /// Экспорт вне манифеста вызывающего либо вызов не по стартовой сессии.
        Refused = 2,
    }
}

/// Идентификатор протокола во владеющем буфере не длиннее [`PROTOCOL_ID_MAX`] байт.
#[derive(Clone, PartialEq, Eq)]
pub struct ProtocolName {
    bytes: [u8; PROTOCOL_ID_MAX],
    len: u8,
}

const _: () = assert!(
    PROTOCOL_ID_MAX <= u8::MAX as usize,
    "protocol id length fits a byte"
);

impl ProtocolName {
    /// `None`, если `protocol` длиннее [`PROTOCOL_ID_MAX`] байт.
    #[must_use]
    pub fn new(protocol: &str) -> Option<Self> {
        let len = u8::try_from(protocol.len()).ok()?;
        let mut bytes = [0; PROTOCOL_ID_MAX];
        bytes
            .get_mut(..protocol.len())?
            .copy_from_slice(protocol.as_bytes());

        Some(Self { bytes, len })
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        // Байты записаны из `&str` в `new`, и разбор UTF-8 здесь не отказывает.
        self.bytes
            .get(..usize::from(self.len))
            .and_then(|bytes| core::str::from_utf8(bytes).ok())
            .unwrap_or_default()
    }
}

impl fmt::Debug for ProtocolName {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("ProtocolName")
            .field(&self.as_str())
            .finish()
    }
}

impl<'a, R: ReceivedResources> WireValue<'a, R> for ProtocolName {
    type Owned = Self;

    fn from_field(data: &'a [u8], _frame: &mut R) -> Result<Self, IpcError> {
        Self::new(value::decode_str(data)?).ok_or(IpcError::InvalidValue)
    }
}

impl<R: ReceivedResources> IntoWireValue<R> for ProtocolName {
    type Read = Self;

    fn write_into(self, frame: &mut MessageBuf<R>, field_id: u8) -> Result<(), IpcError> {
        frame.write_field(field_id, self.as_str().as_bytes())
    }
}

impl WireTyped for ProtocolName {
    const WIRE_TYPE: WireType = WireType::BoundedStr(PROTOCOL_ID_MAX);
}

ipc::wire_value! {
    /// Имя требования и протокол сессии.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct SessionRequirement<'a> {
        pub name: &'a str,
        pub protocol: &'a str,
    }
}

ipc::wire_value! {
    /// Заявка импортёра на экспорт провайдера.
    #[derive(Debug)]
    pub struct SessionRequest<R = ipc::Reply<Result<ipc::wire::Cap<kind::Port>, SessionError>>> {
        /// Ответ на операцию `session` либо `open` импортёра.
        pub reply: R,
        /// Атрибуты запроса импортёра, включая пары вне совпавшего фильтра. Для `open` -
        /// атрибуты запроса `all`, нашедшего экспорт. Запрос без атрибутов даёт пустой набор.
        pub attributes: AttributeSet,
    }
}

ipc::wire_value! {
    /// Результат поиска: сессия родителя, закреплённая за найденным экспортом, протокол
    /// экспорта и первый в порядке объявления фильтр, совпавший с запросом.
    #[derive(Debug)]
    pub struct Found<S = ipc::wire::Cap<kind::Port>> {
        pub session: S,
        pub protocol: ProtocolName,
        pub filter: AttributeSet,
    }
}

const _: () = assert!(
    max_body_len(&[
        WireType::Aggregate(&[
            WireType::BoundedStr(REQUIREMENT_NAME_MAX),
            WireType::BoundedStr(PROTOCOL_ID_MAX),
        ]),
        WireType::BoundedBytes(ATTRIBUTES_MAX_BYTES),
    ]) <= BODY_MAX,
    "session request frame exceeds the body budget"
);

const _: () = assert!(
    max_body_len(&[<Result<SessionRequest, AcceptError> as WireTyped>::WIRE_TYPE]) <= BODY_MAX,
    "accept response frame exceeds the body budget"
);

const _: () = assert!(
    max_body_len(&[<Result<Option<Found>, SessionError> as WireTyped>::WIRE_TYPE]) <= BODY_MAX,
    "next response frame exceeds the body budget"
);

ipc::protocol! {
    /// Стартовая сессия ребёнка к родителю.
    ///
    /// Стартовая сессия допускает `session`, `all` и `accept`, сессия импортёра - `session` и
    /// `all`, сессия перечисления - `next`, сессия результата поиска - `open`.
    /// Остальные операции получают `Refused`.
    pub mod component_startup {
        name = "ComponentStartup";

        /// Открывает сессию единственного экспорта протокола `requirement.protocol`, совпавшего с
        /// `attributes`. Экспорт, скрытый до публикации признака машины, не совпадает. Без
        /// совпадений ответ `Unbound`, при нескольких - `Refused`. Заявки импортёра на один
        /// экспорт по одному требованию ждут независимо; сверх предела родителя самая давняя
        /// получает `NoSpace`.
        call(defer) fn session(
            requirement: SessionRequirement,
            attributes: QueryAttributes,
        ) -> own Result<ipc::wire::Cap<kind::Port>, SessionError>;

        /// Выдаёт заявку импортёра на экспорт `export`. Импортёр получает ответ на заявку как
        /// ответ своей операции `session` либо `open`.
        call(defer) fn accept(
            export: &str,
        ) -> own Result<SessionRequest, AcceptError>;

        /// Выдаёт сессию перечисления экспортов протокола `requirement.protocol`, совпавших с
        /// `attributes` на момент вызова. Требование объявлено видом `all`. Перечисление не
        /// открывает сессий экспортов. Отсутствие совпадений даёт пустое перечисление.
        call fn all(
            requirement: SessionRequirement,
            attributes: QueryAttributes,
        ) -> own Result<ipc::wire::Cap<kind::Port>, SessionError>;

        /// Выдаёт следующий результат перечисления либо `None` в его конце. Экспорт, снятый
        /// навсегда до выдачи, пропускается.
        call(defer) fn next() -> own Result<Option<Found>, SessionError>;

        /// Открывает сессию найденного экспорта и ждёт ответа провайдера. Окончательно удалённый
        /// провайдер даёт `Unavailable`, другой экспорт не подставляется.
        call(defer) fn open() -> own Result<ipc::wire::Cap<kind::Port>, SessionError>;
    }
}

#[cfg(test)]
mod tests {
    use std::{string::String, vec::Vec};

    use ipc::wire::{Cap, IpcError};
    use ipc_test::{MockHandle, MockReply, MockResources};
    use syscall::kind;

    use super::{
        AcceptError, Found, PROTOCOL_ID_MAX, ProtocolName, QueryAttributes, SessionError,
        SessionRequest, SessionRequirement, component_startup,
    };
    use crate::{
        ATTRIBUTE_COUNT_MAX, ATTRIBUTE_KEY_MAX_BYTES, ATTRIBUTE_VALUE_MAX_BYTES, Attribute,
        AttributeSet, Attributes, attributes_len, encode_attributes_into,
    };

    const SESSION_CAP: MockHandle<kind::Port> = MockHandle::new(0x28);
    const FOUND_CAP: MockHandle<kind::Port> = MockHandle::new(0x29);
    const EXPORT: &str = "FileSystem";
    const REQUIREMENT: &str = "files";

    #[derive(Debug, PartialEq, Eq)]
    struct RequestedSession {
        name: String,
        protocol: String,
        attributes: Vec<u8>,
    }

    #[derive(Default)]
    struct Parent {
        requirements: Vec<RequestedSession>,
        exports: Vec<String>,
        waiting: Option<(component_startup::session::Reply<MockReply>, AttributeSet)>,
        filter: Vec<u8>,
    }

    impl component_startup::Service<MockResources> for Parent {
        fn session(
            &mut self,
            _frame: &MockResources,
            requirement: SessionRequirement<'_>,
            attributes: QueryAttributes<'_>,
            reply: component_startup::session::Reply<MockReply>,
        ) {
            self.requirements.push(RequestedSession {
                name: requirement.name.into(),
                protocol: requirement.protocol.into(),
                attributes: attributes.into(),
            });
            match Attributes::decode(attributes) {
                Some(attributes) => self.waiting = Some((reply, AttributeSet::new(attributes))),
                None => {
                    let _ = reply.send(Err::<MockHandle<kind::Port>, _>(SessionError::Refused));
                }
            }
        }

        fn accept(
            &mut self,
            _frame: &MockResources,
            export: &str,
            reply: component_startup::accept::Reply<MockReply>,
        ) {
            self.exports.push(export.into());
            let Some((debt, attributes)) = self.waiting.take() else {
                let _ = reply.send(Err::<
                    SessionRequest<component_startup::session::Reply<MockReply>>,
                    _,
                >(AcceptError::Refused));
                return;
            };
            reply
                .send(Ok::<_, AcceptError>(SessionRequest {
                    reply: debt,
                    attributes,
                }))
                .expect("the parent answers accept");
        }

        fn all(
            &mut self,
            _frame: &MockResources,
            _requirement: SessionRequirement<'_>,
            _attributes: QueryAttributes<'_>,
        ) -> impl ipc::IntoWireValue<MockResources, Read = Result<Cap<kind::Port>, SessionError>>
        {
            Err::<MockHandle<kind::Port>, _>(SessionError::Refused)
        }

        fn next(
            &mut self,
            _frame: &MockResources,
            reply: component_startup::next::Reply<MockReply>,
        ) {
            let filter = Attributes::decode(&self.filter).expect("filter decodes");
            let protocol = "P".repeat(PROTOCOL_ID_MAX);

            reply
                .send(Ok::<_, SessionError>(Some(Found {
                    session: FOUND_CAP,
                    protocol: ProtocolName::new(&protocol).expect("protocol at the bound"),
                    filter: AttributeSet::new(filter),
                })))
                .expect("the parent answers next");
        }

        fn open(
            &mut self,
            _frame: &MockResources,
            reply: component_startup::open::Reply<MockReply>,
        ) {
            let _ = reply.send(Err::<MockHandle<kind::Port>, _>(SessionError::Unavailable));
        }
    }

    ipc::wire_value! {
        struct IndexedRequirement<'a> {
            name: &'a str,
            protocol: &'a str,
            index: u16,
        }
    }

    ipc::protocol! {
        mod indexed_startup {
            name = "ComponentStartup";

            call(defer) fn session(
                requirement: IndexedRequirement,
            ) -> own Result<ipc::wire::Cap<kind::Port>, super::SessionError>;
        }
    }

    fn maximal_attributes() -> Vec<u8> {
        let keys: Vec<_> = (0..ATTRIBUTE_COUNT_MAX)
            .map(|index| std::vec![u8::try_from(index).unwrap(); ATTRIBUTE_KEY_MAX_BYTES])
            .collect();
        let value = [0xff; ATTRIBUTE_VALUE_MAX_BYTES];
        let pairs: Vec<_> = keys
            .iter()
            .map(|key| Attribute { key, value: &value })
            .collect();

        let mut bytes = std::vec![0; attributes_len(&pairs)];
        encode_attributes_into(&mut bytes, &pairs);
        bytes
    }

    #[test]
    fn an_accept_response_carries_the_reply_and_attributes_of_the_request() {
        let (importer_end, importer_listener) = ipc_test::pair();
        let (provider_end, provider_listener) = ipc_test::pair();
        let mut parent = Parent::default();
        let attributes = maximal_attributes();
        let name = "n".repeat(64);
        let protocol = "p".repeat(PROTOCOL_ID_MAX);

        let answer = std::thread::scope(|scope| {
            let importer = scope.spawn(|| {
                let requirement = SessionRequirement {
                    name: &name,
                    protocol: &protocol,
                };
                component_startup::Client::new(importer_end).session(requirement, &attributes)
            });
            importer_listener
                .serve(|frame| component_startup::dispatch(&mut parent, frame))
                .expect("dispatch ok");

            let provider =
                scope.spawn(|| component_startup::Client::new(provider_end).accept(EXPORT));
            provider_listener
                .serve(|frame| component_startup::dispatch(&mut parent, frame))
                .expect("dispatch ok");

            let request = provider
                .join()
                .expect("provider joins")
                .expect("a request arrives");
            assert_eq!(request.attributes.as_attributes().as_bytes(), attributes);
            let reply: ipc::Reply<Result<Cap<kind::Port>, SessionError>, MockReply> = request.reply;
            reply
                .send(Ok::<_, SessionError>(SESSION_CAP))
                .expect("the provider answers");
            importer.join().expect("importer joins")
        });

        assert_eq!(answer, Ok(SESSION_CAP));
        assert_eq!(
            parent.requirements,
            [RequestedSession {
                name,
                protocol,
                attributes,
            }]
        );
        assert_eq!(parent.exports, [String::from(EXPORT)]);
    }

    #[test]
    fn a_found_result_carries_a_maximal_filter_and_protocol() {
        let (end, listener) = ipc_test::pair();
        let mut parent = Parent {
            filter: maximal_attributes(),
            ..Parent::default()
        };

        let found = std::thread::scope(|scope| {
            let enumeration = scope.spawn(|| component_startup::Client::new(end).next());
            listener
                .serve(|frame| component_startup::dispatch(&mut parent, frame))
                .expect("dispatch ok");
            enumeration.join().expect("client joins")
        });

        let found = found.expect("a result").expect("not the end");

        assert_eq!(found.session, FOUND_CAP);
        assert_eq!(found.protocol.as_str(), "P".repeat(PROTOCOL_ID_MAX));
        assert_eq!(found.filter.as_attributes().as_bytes(), parent.filter);
    }

    #[test]
    fn a_session_frame_with_an_index_instead_of_attributes_is_refused() {
        for index in [0, 7] {
            let (end, listener) = ipc_test::pair();
            let mut parent = Parent::default();

            let answer = std::thread::scope(|scope| {
                let importer = scope.spawn(|| {
                    indexed_startup::Client::new(end).session(IndexedRequirement {
                        name: REQUIREMENT,
                        protocol: EXPORT,
                        index,
                    })
                });

                let dispatched =
                    listener.serve(|frame| component_startup::dispatch(&mut parent, frame));
                assert!(dispatched.is_err());
                importer.join().expect("importer joins")
            });

            assert_eq!(answer, Err(ipc::CallError::Ipc(IpcError::PeerClosed)));
            assert!(parent.requirements.is_empty());
        }
    }
}
