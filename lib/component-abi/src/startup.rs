//! Протокол [`component_startup`]. Компонент запрашивает им у родителя ресурсы по имени
//! требования, а провайдер принимает им заявки импортёров на свой экспорт.
//!
//! Код выхода 0 сообщает родителю, что компонент закончил работу, ненулевой код - об отказе.

use core::fmt;

use ipc::{
    IntoWireValue, ReceivedResources, WireTyped, WireValue,
    schema::{WireType, max_body_len},
    wire::{BODY_MAX, ENUM_DISCRIMINANT_WIDTH, IpcError, MessageBuf, Str, value},
};
use syscall::kind;

/// Граница длины имени требования в байтах.
pub const REQUIREMENT_NAME_MAX: usize = 64;

/// Срок одного запроса компонента к родителю по стартовой сессии.
pub const STARTUP_REQUEST_TIMEOUT: ipc::Timeout = ipc::Timeout::from_secs(5);

/// Граница длины идентификатора протокола [`ipc::Protocol::ID`] в байтах.
pub const PROTOCOL_ID_MAX: usize = 64;

/// Граница длины области сессии в байтах. Смысл области принадлежит провайдеру. Контракт
/// компонента держит её строкой в этой границе.
pub const SESSION_SCOPE_MAX: usize = 255;

/// Имя, под которым компонент запрашивает сессию журнала с протоколом `Log`. Манифест это имя
/// не объявляет. Родитель без журнала отвечает на запрос `Refused`.
pub const LOG_REQUIREMENT: &str = "log";

macro_rules! discriminant_error {
    (
        $(#[$attr:meta])*
        pub enum $name:ident {
            $( $(#[$variant_attr:meta])* $variant:ident = $value:literal, )+
        }
    ) => {
        $(#[$attr])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub enum $name {
            $( $(#[$variant_attr])* $variant, )+
        }

        impl $name {
            #[cfg(test)]
            const VARIANTS: &[Self] = &[$( Self::$variant ),+];

            const fn discriminant(self) -> u16 {
                match self {
                    $( Self::$variant => $value, )+
                }
            }

            const fn from_discriminant(value: u16) -> Option<Self> {
                match value {
                    $( $value => Some(Self::$variant), )+
                    _ => None,
                }
            }
        }

        impl<'a, R: ReceivedResources> WireValue<'a, R> for $name {
            type Owned = Self;

            fn from_field(data: &'a [u8], _frame: &mut R) -> Result<Self, IpcError> {
                Self::from_discriminant(value::decode_discriminant(data)?)
                    .ok_or(IpcError::InvalidValue)
            }
        }

        impl<R: ReceivedResources> IntoWireValue<R> for $name {
            type Read = Self;

            fn write_into(self, frame: &mut MessageBuf<R>, field_id: u8) -> Result<(), IpcError> {
                frame.write_field(field_id, &value::encode_discriminant(self.discriminant()))
            }
        }

        impl WireTyped for $name {
            const WIRE_TYPE: WireType = WireType::Uint(ENUM_DISCRIMINANT_WIDTH);
        }
    };
}

discriminant_error! {
    /// Отказ в сессии требования. `Refused` и `Unavailable` окончательны.
    pub enum SessionError {
        /// Повтор запроса заменил заявку либо не хватило ресурса.
        NoSpace = 1,
        /// Имя, вид либо протокол вне манифеста вызывающего, либо провайдер не допустил область.
        Refused = 2,
        /// Провайдер требования снят навсегда.
        Unavailable = 3,
    }
}

discriminant_error! {
    /// Отказ в заявке на экспорт.
    pub enum AcceptError {
        /// Повтор `accept` тем же провайдером заменил этот запрос.
        NoSpace = 1,
        /// Экспорт вне манифеста вызывающего либо вызов не по стартовой сессии.
        Refused = 2,
    }
}

discriminant_error! {
    /// Отказ в регионе образа.
    pub enum ResourceError {
        /// Копия capability не создана.
        NoSpace = 1,
        /// Имя вне манифеста вызывающего либо требование другого вида.
        Refused = 2,
    }
}

/// Область сессии - строка UTF-8 не длиннее [`SESSION_SCOPE_MAX`] байт. Пустая область
/// называет весь экспорт.
#[derive(Clone, PartialEq, Eq)]
pub struct SessionScope {
    bytes: [u8; SESSION_SCOPE_MAX],
    len: u8,
}

const _: () = assert!(
    SESSION_SCOPE_MAX <= u8::MAX as usize,
    "scope length fits a byte"
);

impl SessionScope {
    /// Область из строки `scope`. `None`, если строка длиннее [`SESSION_SCOPE_MAX`] байт.
    #[must_use]
    pub fn new(scope: &str) -> Option<Self> {
        let len = u8::try_from(scope.len()).ok()?;
        let mut bytes = [0; SESSION_SCOPE_MAX];
        bytes
            .get_mut(..scope.len())?
            .copy_from_slice(scope.as_bytes());
        Some(Self { bytes, len })
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        // Байты области записаны из `&str` в `new`, и разбор UTF-8 здесь не отказывает.
        self.bytes
            .get(..usize::from(self.len))
            .and_then(|bytes| core::str::from_utf8(bytes).ok())
            .unwrap_or_default()
    }
}

impl fmt::Debug for SessionScope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("SessionScope").field(&self.as_str()).finish()
    }
}

impl<'a, R: ReceivedResources> WireValue<'a, R> for SessionScope {
    type Owned = Self;

    fn from_field(data: &'a [u8], _frame: &mut R) -> Result<Self, IpcError> {
        Self::new(value::decode_str(data)?).ok_or(IpcError::InvalidValue)
    }
}

impl<R: ReceivedResources> IntoWireValue<R> for SessionScope {
    type Read = Self;

    fn write_into(self, frame: &mut MessageBuf<R>, field_id: u8) -> Result<(), IpcError> {
        frame.write_field(field_id, self.as_str().as_bytes())
    }
}

impl WireTyped for SessionScope {
    const WIRE_TYPE: WireType = WireType::BoundedStr(SESSION_SCOPE_MAX);
}

ipc::wire_value! {
    /// Имя требования и протокол запрошенной сессии.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct SessionRequirement<'a> {
        pub name: Str<'a, REQUIREMENT_NAME_MAX>,
        pub protocol: Str<'a, PROTOCOL_ID_MAX>,
    }
}

ipc::wire_value! {
    /// Заявка импортёра на экспорт провайдера.
    #[derive(Debug)]
    pub struct SessionRequest<R = ipc::Reply<Result<ipc::wire::Cap<kind::Port>, SessionError>>> {
        /// Область, которую назвал родитель.
        pub scope: SessionScope,
        /// Ответ на операцию `session` импортёра.
        pub reply: R,
    }
}

const _: () = assert!(
    max_body_len(&[<SessionRequirement<'static> as WireTyped>::WIRE_TYPE]) <= BODY_MAX,
    "session request frame exceeds the body budget"
);

const _: () = assert!(
    max_body_len(&[<Result<SessionRequest, AcceptError> as WireTyped>::WIRE_TYPE]) <= BODY_MAX,
    "accept response frame exceeds the body budget"
);

ipc::protocol! {
    /// Стартовая сессия ребёнка к родителю.
    pub mod component_startup {
        name = "ComponentStartup";

        /// Выдаёт сессию требования `requirement.name` протокола `requirement.protocol`. Провайдер
        /// отвечает на запрос через `accept`, либо родитель отвечает сам. Повтор запроса заменяет
        /// ожидающую заявку, и она получает `NoSpace`.
        call(defer) fn session(
            requirement: SessionRequirement,
        ) -> own Result<ipc::wire::Cap<kind::Port>, SessionError>;

        /// Выдаёт регион образа требования `requirement` на чтение до конца incarnation
        /// вызывающего.
        call(defer) fn region(
            requirement: Str<{ REQUIREMENT_NAME_MAX }>,
        ) -> own Result<ipc::wire::Cap<kind::MemoryRegion>, ResourceError>;

        /// Выдаёт заявку импортёра на экспорт `export`. Импортёр получает ответ на заявку как
        /// ответ своей операции `session`.
        call(defer) fn accept(
            export: Str<{ PROTOCOL_ID_MAX }>,
        ) -> own Result<SessionRequest, AcceptError>;
    }
}

#[cfg(test)]
mod tests {
    use std::{string::String, vec::Vec};

    use ipc::{
        IntoWireValue,
        wire::{Cap, FieldCursor, IpcError, MessageBuf, Str},
    };
    use ipc_test::{MockHandle, MockReply, MockResources};
    use syscall::kind;

    use super::{
        AcceptError, PROTOCOL_ID_MAX, ResourceError, SESSION_SCOPE_MAX, SessionError,
        SessionRequest, SessionRequirement, SessionScope, WireValue, component_startup, value,
    };

    const SESSION_CAP: MockHandle<kind::Port> = MockHandle::new(0x28);
    const SCOPE: &str = "/boot/demo";
    const EXPORT: &str = "FileSystem";
    const REQUIREMENT: &str = "files";

    const FIELD_ID: u8 = 1;

    #[derive(Default)]
    struct Parent {
        requirements: Vec<(String, String)>,
        exports: Vec<String>,
        waiting: Option<component_startup::session::Reply<MockReply>>,
    }

    impl component_startup::Service<MockResources> for Parent {
        fn session(
            &mut self,
            _frame: &MockResources,
            requirement: SessionRequirement<'_>,
            reply: component_startup::session::Reply<MockReply>,
        ) {
            self.requirements.push((
                requirement.name.as_str().into(),
                requirement.protocol.as_str().into(),
            ));
            self.waiting = Some(reply);
        }

        fn region(
            &mut self,
            _frame: &MockResources,
            _requirement: Str<'_, { super::REQUIREMENT_NAME_MAX }>,
            _reply: component_startup::region::Reply<MockReply>,
        ) {
        }

        fn accept(
            &mut self,
            _frame: &MockResources,
            export: Str<'_, PROTOCOL_ID_MAX>,
            reply: component_startup::accept::Reply<MockReply>,
        ) {
            self.exports.push(export.as_str().into());
            let Some(debt) = self.waiting.take() else {
                let _ = reply.send(Err::<
                    SessionRequest<component_startup::session::Reply<MockReply>>,
                    _,
                >(AcceptError::Refused));
                return;
            };
            let scope = SessionScope::new(SCOPE).expect("scope within the bound");
            reply
                .send(Ok::<_, AcceptError>(SessionRequest { scope, reply: debt }))
                .expect("the parent answers accept");
        }
    }

    fn field_of(data: &[u8]) -> MessageBuf {
        let mut buf = MessageBuf::new();
        buf.write_field(FIELD_ID, data).expect("write ok");
        buf.finish().expect("finish ok");
        buf
    }

    fn read_field<T>(buf: &MessageBuf) -> Result<T, IpcError>
    where
        T: for<'a> WireValue<'a, (), Owned = T>,
    {
        let mut cursor = FieldCursor::new(buf.as_bytes());
        let field = cursor.next_field().expect("decode ok").expect("present");
        T::from_field(field.data, &mut ())
    }

    fn round_trip<T>(written: T) -> Result<T, IpcError>
    where
        T: IntoWireValue<()> + for<'a> WireValue<'a, (), Owned = T>,
    {
        let mut buf = MessageBuf::new();
        written.write_into(&mut buf, FIELD_ID).expect("write ok");
        buf.finish().expect("finish ok");
        read_field(&buf)
    }

    fn outside<T>(variants: &[T]) -> Result<T, IpcError>
    where
        T: for<'a> WireValue<'a, (), Owned = T>,
    {
        let discriminant = u16::try_from(variants.len() + 1).expect("few variants");
        read_field(&field_of(&value::encode_discriminant(discriminant)))
    }

    #[test]
    fn every_variant_round_trips() {
        for variant in SessionError::VARIANTS {
            assert_eq!(round_trip(*variant), Ok(*variant));
        }
        for variant in AcceptError::VARIANTS {
            assert_eq!(round_trip(*variant), Ok(*variant));
        }
        for variant in ResourceError::VARIANTS {
            assert_eq!(round_trip(*variant), Ok(*variant));
        }
    }

    #[test]
    fn a_discriminant_outside_the_set_is_rejected() {
        assert_eq!(outside(SessionError::VARIANTS), Err(IpcError::InvalidValue));
        assert_eq!(outside(AcceptError::VARIANTS), Err(IpcError::InvalidValue));
        assert_eq!(
            outside(ResourceError::VARIANTS),
            Err(IpcError::InvalidValue)
        );
    }

    #[test]
    fn an_accept_response_carries_the_scope_and_the_reply_of_the_request() {
        let (importer_end, importer_listener) = ipc_test::pair();
        let (provider_end, provider_listener) = ipc_test::pair();
        let mut parent = Parent::default();

        let answer = std::thread::scope(|scope| {
            let importer = scope.spawn(|| {
                let requirement = SessionRequirement {
                    name: Str::new(REQUIREMENT).expect("name within the bound"),
                    protocol: Str::new(EXPORT).expect("protocol within the bound"),
                };
                component_startup::Client::new(importer_end).session(requirement)
            });
            importer_listener
                .serve(|frame| component_startup::dispatch(&mut parent, frame))
                .expect("dispatch ok");

            let provider = scope.spawn(|| {
                component_startup::Client::new(provider_end)
                    .accept(Str::new(EXPORT).expect("export within the bound"))
            });
            provider_listener
                .serve(|frame| component_startup::dispatch(&mut parent, frame))
                .expect("dispatch ok");

            let request = provider
                .join()
                .expect("provider joins")
                .expect("accept call")
                .expect("a request arrives");
            assert_eq!(request.scope.as_str(), SCOPE);
            let reply: ipc::Reply<Result<Cap<kind::Port>, SessionError>, MockReply> = request.reply;
            reply
                .send(Ok::<_, SessionError>(SESSION_CAP))
                .expect("the provider answers");
            importer.join().expect("importer joins")
        });

        assert_eq!(answer, Ok(Ok(SESSION_CAP)));
        assert_eq!(parent.requirements, [(REQUIREMENT.into(), EXPORT.into())]);
        assert_eq!(parent.exports, [String::from(EXPORT)]);
    }

    #[test]
    fn an_empty_scope_names_the_whole_export() {
        let empty = SessionScope::new("").expect("empty scope");

        assert_eq!(empty.as_str(), "");
        assert_eq!(round_trip(empty.clone()), Ok(empty));
    }

    #[test]
    fn a_scope_at_the_bound_round_trips() {
        let text = "a".repeat(SESSION_SCOPE_MAX);
        let scope = SessionScope::new(&text).expect("scope at the bound");

        let read = round_trip(scope).expect("scope reads");
        assert_eq!(read.as_str(), text);
    }

    #[test]
    fn a_scope_past_the_bound_is_refused() {
        let text = "a".repeat(SESSION_SCOPE_MAX + 1);

        assert_eq!(SessionScope::new(&text), None);
        assert_eq!(
            read_field::<SessionScope>(&field_of(text.as_bytes())),
            Err(IpcError::InvalidValue)
        );
    }

    #[test]
    fn a_scope_that_is_not_utf8_is_refused() {
        assert_eq!(
            read_field::<SessionScope>(&field_of(&[0xFF])),
            Err(IpcError::InvalidValue)
        );
    }
}
