//! Контракт `Supervisor` для расширения композиции: подача записей плана и получение сессии
//! поднятого компонента.
//!
//! Клиента и сервис даёт модуль [`supervisor`]. Расширение идёт транзакцией: `begin`
//! открывает её, `define` подаёт запись плана вместе с capability на регион программы,
//! `commit` применяет накопленное. Отказ любого шага назван [`ExtendError`].
//!
//! Операция `session` отдаёт capability сессии экспорта компонента по имени его экземпляра и
//! протоколу этого экспорта. Границы записи плана и имён заданы [`PLAN_RECORD_MAX`],
//! [`INSTANCE_NAME_MAX`] и [`PROTOCOL_NAME_MAX`].

#![cfg_attr(not(test), no_std)]
#![cfg_attr(
    test,
    allow(clippy::cast_possible_truncation, clippy::unwrap_in_result)
)]

#[cfg(test)]
extern crate std;

use ipc::{
    WireTyped, WireValue,
    schema::WireType,
    wire::{ENUM_DISCRIMINANT_WIDTH, HandleSlots, IpcError, MessageBuf, value},
};
use syscall::kind;

/// Предел записи плана одного компонента в байтах.
pub const PLAN_RECORD_MAX: usize = 2048;

/// Предел имени экземпляра в байтах. Под этим именем идёт процесс компонента.
pub const INSTANCE_NAME_MAX: usize = 64;

/// Предел имени протокола в байтах. Протокол называет экспорт компонента.
pub const PROTOCOL_NAME_MAX: usize = 64;

/// Причина, по которой супервизор отклонил операцию транзакции.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExtendError {
    /// Транзакция расширения у вызывателя не открыта.
    NoTransaction,
    /// Запись плана не разобрана, содержит не один компонент либо capability на
    /// регион программы не принята.
    BadRecord,
    /// Имя экземпляра занято другим компонентом композиции.
    Duplicate,
    /// Ребро набора не разрешается в компонент композиции либо замыкает порядок запуска.
    Unresolved,
    /// Ресурса под набор не хватило.
    NoSpace,
    /// Компонент набора не поднялся.
    NotStarted,
    /// Компонента с таким именем вызыватель не определял либо названного протокола нет
    /// среди его экспортов.
    NotFound,
}

const VARIANTS: [ExtendError; 7] = [
    ExtendError::NoTransaction,
    ExtendError::BadRecord,
    ExtendError::Duplicate,
    ExtendError::Unresolved,
    ExtendError::NoSpace,
    ExtendError::NotStarted,
    ExtendError::NotFound,
];

ipc::protocol! {
    /// Расширение композиции: компонент подаёт супервизору записи плана и регионы их программ,
    /// а супервизор поднимает набор целиком либо не поднимает ничего.
    pub mod supervisor {
        name = "Supervisor";

        /// Открывает транзакцию расширения. Набор, оставшийся от предыдущей транзакции
        /// вызывателя, снимается вместе с регионами его программ.
        call fn begin() -> Result<u32, ExtendError>;

        /// Кладёт в транзакцию запись плана одного компонента вместе с регионом, чьи байты
        /// начинаются программой. Регион отображается read-only, и слак страницы за концом
        /// программы разбору не мешает.
        call fn define(
            record: ipc::wire::Bytes<{ PLAN_RECORD_MAX }>,
            program: Cap<kind::MemoryRegion>,
        ) -> Result<u32, ExtendError>;

        /// Проверяет набор целиком и поднимает его в топологическом порядке. Отказ снимает
        /// транзакцию, не поднимая ни одного компонента набора.
        call fn commit() -> Result<u32, ExtendError>;

        /// Отдаёт сессию экспорта `protocol` компонента `component` тем же путём, каким
        /// супервизор наполняет слот `service`. Компонент, которого вызыватель не
        /// определял, и протокол вне его экспортов - `NotFound`.
        call fn session(
            component: ipc::wire::Str<{ INSTANCE_NAME_MAX }>,
            protocol: ipc::wire::Str<{ PROTOCOL_NAME_MAX }>,
        ) -> Result<ipc::wire::Cap<kind::Port>, ExtendError>;
    }
}

impl ExtendError {
    const fn discriminant(self) -> u16 {
        match self {
            Self::NoTransaction => 1,
            Self::BadRecord => 2,
            Self::Duplicate => 3,
            Self::Unresolved => 4,
            Self::NoSpace => 5,
            Self::NotStarted => 6,
            Self::NotFound => 7,
        }
    }

    fn from_discriminant(value: u16) -> Option<Self> {
        VARIANTS
            .into_iter()
            .find(|variant| variant.discriminant() == value)
    }
}

impl<'a> WireValue<'a> for ExtendError {
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

impl WireTyped for ExtendError {
    const WIRE_TYPE: WireType = WireType::Uint(ENUM_DISCRIMINANT_WIDTH);
}

#[cfg(test)]
mod tests {
    use core::num::NonZeroU32;
    use std::{string::String, vec::Vec};

    use ipc::wire::{Bytes, Cap, HandleSlots, MESSAGE_INLINE_MAX, MessageBuf, Str};
    use ipc_test::MockEnd;
    use syscall::{Handle, ObjectKind, kind};

    use super::{
        ExtendError, INSTANCE_NAME_MAX, IpcError, PLAN_RECORD_MAX, PROTOCOL_NAME_MAX, VARIANTS,
        WireValue, supervisor, value,
    };

    const PROGRAM: Cap<kind::MemoryRegion> = cap(0x51);
    const SESSION: Cap<kind::Port> = cap(0x52);

    const FIELD_ID: u8 = 1;

    const RECORD: &[u8] = b"RKPLAN\0\0record";
    const DEFINED: &str = "extended-ramfs";
    const EXPORT: &str = "FileSystem";

    #[derive(Default)]
    struct Supervisor {
        open: bool,
        defined: Vec<String>,
    }

    const fn cap<K: ObjectKind>(raw: u32) -> Cap<K> {
        match NonZeroU32::new(raw) {
            Some(id) => Cap::from_handle(Handle::from_abi(id)),
            None => panic!("non-zero"),
        }
    }

    impl supervisor::Service for Supervisor {
        fn begin(&mut self) -> Result<u32, ExtendError> {
            self.open = true;
            self.defined.clear();
            Ok(0)
        }

        fn define(
            &mut self,
            record: Bytes<'_, PLAN_RECORD_MAX>,
            program: Cap<kind::MemoryRegion>,
        ) -> Result<u32, ExtendError> {
            if !self.open {
                return Err(ExtendError::NoTransaction);
            }
            if record.as_bytes() != RECORD || program != PROGRAM {
                return Err(ExtendError::BadRecord);
            }
            self.defined.push(String::from(DEFINED));
            Ok(0)
        }

        fn commit(&mut self) -> Result<u32, ExtendError> {
            if !self.open {
                return Err(ExtendError::NoTransaction);
            }
            self.open = false;
            Ok(0)
        }

        fn session(
            &mut self,
            component: Str<'_, INSTANCE_NAME_MAX>,
            protocol: Str<'_, PROTOCOL_NAME_MAX>,
        ) -> Result<Cap<kind::Port>, ExtendError> {
            let known = self.defined.iter().any(|name| name == component.as_str());
            if known && protocol.as_str() == EXPORT {
                Ok(SESSION)
            } else {
                Err(ExtendError::NotFound)
            }
        }
    }

    fn round_trip(error: ExtendError) -> Result<ExtendError, IpcError> {
        let mut buf = MessageBuf::<MESSAGE_INLINE_MAX>::new();
        error
            .write_as_field(&mut buf, FIELD_ID, &mut HandleSlots::new())
            .expect("write ok");
        buf.finish().expect("finish ok");
        let mut cursor = ipc::wire::FieldCursor::new(buf.as_bytes());
        let field = cursor.next_field().expect("decode ok").expect("present");
        ExtendError::from_field(field.data, &HandleSlots::new())
    }

    #[test]
    fn a_committed_transaction_vends_the_session_of_the_component_it_defined() {
        let (client_end, server_end) = MockEnd::pair();
        let mut supervisor = Supervisor::default();
        std::thread::scope(|scope| {
            let server = scope.spawn(|| {
                for _ in 0..6 {
                    ipc::Transport::wait_readable(&server_end, ipc::Timeout::INFINITE)
                        .expect("server wait");
                    supervisor::dispatch(&mut supervisor, &server_end).expect("dispatch ok");
                }
            });

            let client = supervisor::Client::new(client_end);
            let record = Bytes::new(RECORD).expect("record within the bound");
            let defined = Str::new(DEFINED).expect("instance name within the bound");
            let absent = Str::new("stage-zero").expect("instance name within the bound");
            let exported = Str::new(EXPORT).expect("protocol name within the bound");
            let foreign = Str::new("Rtc").expect("protocol name within the bound");
            assert_eq!(client.begin().expect("call").value, Ok(0));
            assert_eq!(client.define(record, PROGRAM).expect("call").value, Ok(0));
            assert_eq!(client.commit().expect("call").value, Ok(0));
            let mut session = client.session(defined, exported).expect("call");
            assert_eq!(session.value, Ok(SESSION));
            assert_eq!(session.claim(SESSION), Some(SESSION));
            assert_eq!(
                client.session(absent, exported).expect("call").value,
                Err(ExtendError::NotFound)
            );
            assert_eq!(
                client.session(defined, foreign).expect("call").value,
                Err(ExtendError::NotFound)
            );
            server.join().expect("server joins");
        });
    }

    #[test]
    fn defining_outside_a_transaction_is_refused() {
        let (client_end, server_end) = MockEnd::pair();
        let mut supervisor = Supervisor::default();
        std::thread::scope(|scope| {
            let server = scope.spawn(|| {
                ipc::Transport::wait_readable(&server_end, ipc::Timeout::INFINITE)
                    .expect("server wait");
                supervisor::dispatch(&mut supervisor, &server_end).expect("dispatch ok");
            });

            let client = supervisor::Client::new(client_end);
            let record = Bytes::new(RECORD).expect("record within the bound");
            assert_eq!(
                client.define(record, PROGRAM).expect("call").value,
                Err(ExtendError::NoTransaction)
            );
            server.join().expect("server joins");
        });
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
            ExtendError::from_field(&outside, &HandleSlots::new()),
            Err(IpcError::InvalidValue)
        );
    }
}
