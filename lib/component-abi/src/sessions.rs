//! Контракт установления сессии компонента: сессию своего service-порта заводит он сам, а
//! супервизор просит её по control-порту, называя область, до которой её сузить.

use ipc::{
    WireTyped, WireValue,
    schema::WireType,
    wire::{
        BODY_MAX, ENUM_DISCRIMINANT_WIDTH, FIELD_OVERHEAD, HandleSlots, IpcError, MessageBuf, value,
    },
};

/// Граница длины области сессии в байтах. Смысл области принадлежит провайдеру: контракт
/// компонента держит её строкой в этой границе.
pub const SESSION_SCOPE_MAX: usize = 255;

/// Причина, по которой сессия не установлена.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionError {
    /// Ресурса под сессию не хватило.
    NoSpace,
    /// Политика допуска компонента отказала.
    Refused,
}

const VARIANTS: [SessionError; 2] = [SessionError::NoSpace, SessionError::Refused];

const _: () = assert!(
    // Слагаемое `1` - терминатор тела кадра.
    1 + FIELD_OVERHEAD + SESSION_SCOPE_MAX <= BODY_MAX,
    "open_session frame exceeds the body budget"
);

ipc::protocol! {
    /// Установление сессий компонента владельцем его портов.
    pub mod component_sessions {
        name = "ComponentSessions";

        /// Заводит сессию и отдаёт capability service-порта под её значком. `scope` сужает
        /// сессию до названной области, а пустая область отдаёт экспорт целиком. Значок
        /// назначает компонент: политика допуска принадлежит ему, и место отказа - само
        /// установление.
        ///
        /// Значения значков не переиспользуются: это отделяет отставший кадр ушедшей сессии
        /// от живой.
        call fn open_session(
            scope: ipc::wire::Str<{ SESSION_SCOPE_MAX }>,
        ) -> Result<ipc::wire::Cap, SessionError>;
    }
}

impl SessionError {
    const fn discriminant(self) -> u16 {
        match self {
            Self::NoSpace => 1,
            Self::Refused => 2,
        }
    }

    fn from_discriminant(value: u16) -> Option<Self> {
        VARIANTS
            .into_iter()
            .find(|variant| variant.discriminant() == value)
    }
}

impl<'a> WireValue<'a> for SessionError {
    fn write_as_field<const N: usize>(
        &self,
        buf: &mut MessageBuf<N>,
        field_id: u8,
        _handles: &mut HandleSlots,
    ) -> Result<(), IpcError> {
        buf.write_field(field_id, &value::encode_discriminant(self.discriminant()))
    }

    fn from_field(data: &'a [u8], _handles: &HandleSlots) -> Result<Self, IpcError> {
        Self::from_discriminant(value::decode_discriminant(data)?).ok_or(IpcError::InvalidValue)
    }
}

impl WireTyped for SessionError {
    const WIRE_TYPE: WireType = WireType::Uint(ENUM_DISCRIMINANT_WIDTH);
}

#[cfg(test)]
mod tests {
    use core::num::NonZeroU32;
    use std::{
        string::{String, ToString},
        vec::Vec,
    };

    use ipc::{
        Transport,
        wire::{Cap, HandleSlots, MESSAGE_INLINE_MAX, MessageBuf, Str},
    };
    use ipc_test::MockEnd;

    use super::{
        IpcError, SESSION_SCOPE_MAX, SessionError, VARIANTS, WireValue, component_sessions, value,
    };

    const SESSION_CAP: Cap = cap(0x27);
    const SCOPED_CAP: Cap = cap(0x28);

    const SCOPE: &str = "/boot/demo";

    const FIELD_ID: u8 = 1;

    #[derive(Default)]
    struct Component {
        scopes: Vec<String>,
    }

    impl component_sessions::Service for Component {
        fn open_session(&mut self, scope: Str<'_, SESSION_SCOPE_MAX>) -> Result<Cap, SessionError> {
            self.scopes.push(scope.as_str().to_string());
            // Вторая сессия сверх квоты этой заглушки не заводится.
            if self.scopes.len() > 2 {
                return Err(SessionError::Refused);
            }
            if scope.as_str().is_empty() {
                return Ok(SESSION_CAP);
            }
            Ok(SCOPED_CAP)
        }
    }

    const fn cap(raw: u32) -> Cap {
        Cap::from_raw(NonZeroU32::new(raw).expect("non-zero"))
    }

    fn scope_of(text: &str) -> Str<'_, SESSION_SCOPE_MAX> {
        Str::new(text).expect("scope within the bound")
    }

    fn round_trip(error: SessionError) -> Result<SessionError, IpcError> {
        let mut buf = MessageBuf::<MESSAGE_INLINE_MAX>::new();
        error
            .write_as_field(&mut buf, FIELD_ID, &mut HandleSlots::new())
            .expect("write ok");
        buf.finish().expect("finish ok");
        let mut cursor = ipc::wire::FieldCursor::new(buf.as_bytes());
        let field = cursor.next_field().expect("decode ok").expect("present");
        SessionError::from_field(field.data, &HandleSlots::new())
    }

    #[test]
    fn open_session_carries_its_scope_and_may_refuse() {
        let (client_end, server_end) = MockEnd::pair();
        let mut component = Component::default();
        std::thread::scope(|scope| {
            let server = scope.spawn(|| {
                for _ in 0..3 {
                    server_end
                        .wait_readable(ipc::Timeout::INFINITE)
                        .expect("server wait");
                    component_sessions::dispatch(&mut component, &server_end).expect("dispatch ok");
                }
            });

            let client = component_sessions::Client::new(client_end);
            let mut session = client.open_session(scope_of("")).expect("call");
            assert_eq!(session.value, Ok(SESSION_CAP));
            assert_eq!(session.claim(SESSION_CAP), Some(SESSION_CAP));
            let mut scoped = client.open_session(scope_of(SCOPE)).expect("call");
            assert_eq!(scoped.value, Ok(SCOPED_CAP));
            assert_eq!(scoped.claim(SCOPED_CAP), Some(SCOPED_CAP));
            assert_eq!(
                client.open_session(scope_of(SCOPE)).expect("call").value,
                Err(SessionError::Refused)
            );
            server.join().expect("server joins");
        });

        assert_eq!(component.scopes, ["", SCOPE, SCOPE]);
    }

    #[test]
    fn every_variant_round_trips() {
        for variant in VARIANTS {
            assert_eq!(round_trip(variant), Ok(variant));
        }
    }

    #[test]
    fn a_discriminant_outside_the_set_is_rejected() {
        let outside = value::encode_discriminant(VARIANTS.len() as u16 + 1);
        assert_eq!(
            SessionError::from_field(&outside, &HandleSlots::new()),
            Err(IpcError::InvalidValue)
        );
    }
}
