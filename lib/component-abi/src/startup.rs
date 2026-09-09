//! Порт супервизора: стартовая транзакция компонента и установление сессий после неё.
//!
//! Контракт [`component_startup`] переносит стартовый набор capability базовым кадром,
//! кадром на каждый экспорт и кадром на каждый слот. Принятая транзакция собирается в
//! [`StartedComponent`], а до конца транзакции приём доводит
//! [`receive_startup_transaction`]. По тому же порту супервизор запрашивает у компонента
//! сессию названного экспорта операцией `open_session`.

use ipc::{
    Transport, WireTyped, WireValue,
    schema::WireType,
    wire::{
        BODY_MAX, ENUM_DISCRIMINANT_WIDTH, FIELD_OVERHEAD, HandleSlots, IpcError, MessageBuf, value,
    },
};
use syscall::kind;

/// Граница числа слотов capability одного компонента.
pub const COMPONENT_SLOT_MAX: usize = 8;

/// Граница числа экспортов одного компонента. На каждый экспорт супервизор создаёт порт,
/// и его серверная сторона приходит отдельным кадром транзакции.
pub const COMPONENT_EXPORT_MAX: usize = 4;

/// Граница длины области сессии в байтах. Смысл области принадлежит провайдеру. Контракт
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
    /// Порт супервизора: стартовая транзакция компонента композиции и запросы супервизора
    /// после неё.
    pub mod component_startup {
        name = "ComponentStartup";

        /// Открывает транзакцию: лог-сессия, число серверных сторон портов экспортов и
        /// число слотов, которые придут следом.
        ///
        /// `log` передаёт владеющий хэндл. `export_count` сверх
        /// [`COMPONENT_EXPORT_MAX`](crate::COMPONENT_EXPORT_MAX), `slot_count` сверх
        /// [`COMPONENT_SLOT_MAX`](crate::COMPONENT_SLOT_MAX) и второй базовый кадр ломают
        /// транзакцию.
        cast fn begin(log: Cap<kind::Port>, export_count: u16, slot_count: u16);

        /// Отдаёт серверную сторону порта очередного экспорта в порядке объявления.
        ///
        /// Экспорт до базового кадра, экспорт сверх обещанного `export_count` и экспорт
        /// после первого слота ломают транзакцию.
        cast fn export(capability: Cap<kind::Port>);

        /// Отдаёт capability очередного слота в порядке плана.
        ///
        /// Слот до конца экспортов и слот сверх обещанного `slot_count` ломают транзакцию.
        cast fn slot(capability: Cap<kind::Any>);

        /// Создаёт сессию экспорта `export` и отдаёт клиентскую capability его порта под
        /// значком сессии. `scope` сужает сессию до названной области, а пустая область
        /// отдаёт экспорт целиком. Значок назначается компонентом, и политика допуска
        /// принадлежит ему.
        ///
        /// Отказ приходит значением [`SessionError`](crate::SessionError). `NoSpace`
        /// называет нехватку ресурса под сессию, `Refused` - отказ политики допуска либо
        /// экспорт вне объявленных. Порт после отказа остаётся открытым.
        ///
        /// Значения значков не переиспользуются: это отделяет отставший кадр ушедшей сессии
        /// от живой.
        call fn open_session(
            export: u16,
            scope: ipc::wire::Str<{ SESSION_SCOPE_MAX }>,
        ) -> Result<ipc::wire::Cap<kind::Port>, SessionError>;
    }
}

/// Capability и параметры принятой стартовой транзакции. Семантику позиции
/// экспорта и слота определяет сам компонент.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StartedComponent {
    /// Делегированная log-сессия.
    pub log: ipc::wire::Cap<kind::Port>,
    exports: [Option<ipc::wire::Cap<kind::Port>>; COMPONENT_EXPORT_MAX],
    export_count: u16,
    slots: [Option<ipc::wire::Cap<kind::Any>>; COMPONENT_SLOT_MAX],
    slot_count: u16,
}

/// Приёмник стартовой транзакции: базовый кадр, ровно `export_count` экспортов и ровно
/// `slot_count` слотов.
///
/// Приёмник реализует `component_startup::Service`, и нарушенный порядок кадров даёт `None`
/// из [`receive_startup_transaction`]. Запрос сессии до конца транзакции отвергается.
#[derive(Debug, Default)]
pub struct StartupReceiver {
    started: Option<StartedComponent>,
    exports: u16,
    slots: u16,
    broken: bool,
}

/// Принимает стартовую транзакцию целиком: базовый кадр, следом экспорты в порядке
/// объявления и слоты в порядке плана. `None` - транзакция не сложилась в контракт.
pub fn receive_startup_transaction<T: Transport>(
    transport: &T,
) -> Result<Option<StartedComponent>, IpcError> {
    let mut receiver = StartupReceiver::default();
    while !receiver.is_complete() {
        component_startup::dispatch(&mut receiver, transport)?;
        if receiver.broken {
            return Ok(None);
        }
    }
    Ok(receiver.started)
}

impl StartedComponent {
    fn export(&self, index: u16) -> Option<ipc::wire::Cap<kind::Port>> {
        if index >= self.export_count {
            return None;
        }
        *self.exports.get(usize::from(index))?
    }

    fn slot(&self, index: u16) -> Option<ipc::wire::Cap<kind::Any>> {
        if index >= self.slot_count {
            return None;
        }
        *self.slots.get(usize::from(index))?
    }

    /// Серверные стороны портов экспортов в порядке объявления.
    pub fn exports(&self) -> impl Iterator<Item = ipc::wire::Cap<kind::Port>> {
        let started = *self;
        (0..started.export_count).map_while(move |index| started.export(index))
    }

    /// Capability слотов в порядке плана.
    pub fn slots(&self) -> impl Iterator<Item = ipc::wire::Cap<kind::Any>> {
        let started = *self;
        (0..started.slot_count).map_while(move |index| started.slot(index))
    }

    /// Экспорты транзакции в порядке объявления для манифеста, объявившего `declared`
    /// экспортов. Другое их число даёт `None`.
    pub fn declared_exports(
        &self,
        declared: u16,
    ) -> Option<impl Iterator<Item = ipc::wire::Cap<kind::Port>>> {
        (self.export_count == declared).then(|| self.exports())
    }

    /// Слоты транзакции в порядке плана для манифеста, объявившего `declared` слотов.
    /// Другое число слотов транзакции даёт `None`.
    pub fn declared_slots(
        &self,
        declared: u16,
    ) -> Option<impl Iterator<Item = ipc::wire::Cap<kind::Any>>> {
        (self.slot_count == declared).then(|| self.slots())
    }
}

fn take<K: syscall::ObjectKind>(
    places: &mut [Option<ipc::wire::Cap<K>>],
    index: Option<usize>,
    taken: &mut u16,
    capability: ipc::wire::Cap<K>,
) -> bool {
    let Some(place) = index.and_then(|index| places.get_mut(index)) else {
        return false;
    };
    *place = Some(capability);
    *taken += 1;
    true
}

impl StartupReceiver {
    fn next_export(&mut self) -> Option<usize> {
        let started = self.started.as_ref()?;
        // Слоты идут за экспортами, и вернуться к экспортам транзакция не может.
        let ordered = self.slots == 0;
        let index = usize::from(self.exports);
        (ordered && self.exports < started.export_count && index < COMPONENT_EXPORT_MAX)
            .then_some(index)
    }

    fn next_slot(&mut self) -> Option<usize> {
        let started = self.started.as_ref()?;
        let ordered = self.exports == started.export_count;
        let index = usize::from(self.slots);
        (ordered && self.slots < started.slot_count && index < COMPONENT_SLOT_MAX).then_some(index)
    }

    const fn is_complete(&self) -> bool {
        match &self.started {
            Some(started) => {
                self.exports == started.export_count && self.slots == started.slot_count
            }
            None => false,
        }
    }
}

impl component_startup::Service for StartupReceiver {
    fn begin(&mut self, log: ipc::wire::Cap<kind::Port>, export_count: u16, slot_count: u16) {
        if self.started.is_some()
            || usize::from(export_count) > COMPONENT_EXPORT_MAX
            || usize::from(slot_count) > COMPONENT_SLOT_MAX
        {
            self.broken = true;
            return;
        }
        self.started = Some(StartedComponent {
            log,
            exports: [None; COMPONENT_EXPORT_MAX],
            export_count,
            slots: [None; COMPONENT_SLOT_MAX],
            slot_count,
        });
    }

    fn export(&mut self, capability: ipc::wire::Cap<kind::Port>) {
        let index = self.next_export();
        let Some(started) = self.started.as_mut() else {
            self.broken = true;
            return;
        };
        if !take(&mut started.exports, index, &mut self.exports, capability) {
            self.broken = true;
        }
    }

    fn slot(&mut self, capability: ipc::wire::Cap<kind::Any>) {
        let index = self.next_slot();
        let Some(started) = self.started.as_mut() else {
            self.broken = true;
            return;
        };
        if !take(&mut started.slots, index, &mut self.slots, capability) {
            self.broken = true;
        }
    }

    fn open_session(
        &mut self,
        _export: u16,
        _scope: ipc::wire::Str<'_, SESSION_SCOPE_MAX>,
    ) -> Result<ipc::wire::Cap<kind::Port>, SessionError> {
        // Обслуживание порта начинается за транзакцией, и до её конца у компонента нет
        // ни одной сессии.
        Err(SessionError::Refused)
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
    use core::{marker::PhantomData, num::NonZeroU32};
    use std::{
        cell::Cell,
        rc::Rc,
        string::{String, ToString},
        vec::Vec,
    };

    use ipc::{
        IntoWireHandle, Transport,
        wire::{Cap, FieldCursor, HandleSlots, IpcError, MESSAGE_INLINE_MAX, MessageBuf, Str},
    };
    use ipc_test::MockEnd;
    use syscall::{Handle, ObjectKind, kind};

    use super::{
        SESSION_SCOPE_MAX, SessionError, StartupReceiver, VARIANTS, WireValue,
        component_startup::{self, Service as _},
        receive_startup_transaction, value,
    };

    const LOG_CAP: Cap<kind::Port> = cap(0x21);
    const SERVICE_CAP: Cap<kind::Port> = cap(0x24);
    const ADMIN_CAP: Cap<kind::Port> = cap(0x25);
    const IMAGE_CAP: Cap<kind::Any> = cap(0x26);
    const DEVICE_CAP: Cap<kind::Any> = cap(0x27);
    const SESSION_CAP: Cap<kind::Port> = cap(0x28);
    const SCOPED_CAP: Cap<kind::Port> = cap(0x29);

    const SCOPE: &str = "/boot/demo";

    const FIELD_ID: u8 = 1;

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum Fate {
        Pending,
        Relinquished,
        Dropped,
    }

    struct FakeHandle<K: ObjectKind> {
        id: u32,
        fate: Rc<Cell<Fate>>,
        kind: PhantomData<K>,
    }

    #[derive(Default)]
    struct Component {
        requests: Vec<(u16, String)>,
    }

    impl<K: ObjectKind> IntoWireHandle for FakeHandle<K> {
        type Kind = K;

        fn wire_id(&self) -> u32 {
            self.id
        }

        fn relinquish(self) {
            self.fate.set(Fate::Relinquished);
            core::mem::forget(self);
        }
    }

    impl<K: ObjectKind> Drop for FakeHandle<K> {
        fn drop(&mut self) {
            self.fate.set(Fate::Dropped);
        }
    }

    impl component_startup::Service for Component {
        fn begin(&mut self, _log: Cap<kind::Port>, _export_count: u16, _slot_count: u16) {}

        fn export(&mut self, _capability: Cap<kind::Port>) {}

        fn slot(&mut self, _capability: Cap<kind::Any>) {}

        fn open_session(
            &mut self,
            export: u16,
            scope: Str<'_, SESSION_SCOPE_MAX>,
        ) -> Result<Cap<kind::Port>, SessionError> {
            self.requests.push((export, scope.as_str().to_string()));
            if export != 0 {
                return Err(SessionError::Refused);
            }
            if scope.as_str().is_empty() {
                return Ok(SESSION_CAP);
            }
            Ok(SCOPED_CAP)
        }
    }

    const fn cap<K: ObjectKind>(raw: u32) -> Cap<K> {
        match NonZeroU32::new(raw) {
            Some(id) => Cap::from_handle(Handle::from_abi(id)),
            None => panic!("non-zero"),
        }
    }

    fn fake<K: ObjectKind>(id: u32) -> (FakeHandle<K>, Rc<Cell<Fate>>) {
        let fate = Rc::new(Cell::new(Fate::Pending));
        (
            FakeHandle {
                id,
                fate: Rc::clone(&fate),
                kind: PhantomData,
            },
            fate,
        )
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
        let mut cursor = FieldCursor::new(buf.as_bytes());
        let field = cursor.next_field().expect("decode ok").expect("present");
        SessionError::from_field(field.data, &HandleSlots::new())
    }

    #[test]
    fn a_transaction_carries_the_base_frame_every_export_and_every_slot() {
        let (client_end, server_end) = MockEnd::pair();
        let client = component_startup::Client::new(client_end);
        client.begin(LOG_CAP, 2, 2).expect("begin write");
        client.export(SERVICE_CAP).expect("export write");
        client.export(ADMIN_CAP).expect("export write");
        client.slot(IMAGE_CAP).expect("slot write");
        client.slot(DEVICE_CAP).expect("slot write");

        let started = receive_startup_transaction(&server_end)
            .expect("dispatch ok")
            .expect("transaction complete");

        assert_eq!(started.log, LOG_CAP);
        assert_eq!(
            started.exports().collect::<Vec<_>>(),
            [SERVICE_CAP, ADMIN_CAP]
        );
        assert_eq!(started.slots().collect::<Vec<_>>(), [IMAGE_CAP, DEVICE_CAP]);
    }

    #[test]
    fn a_transaction_without_exports_or_slots_needs_no_further_casts() {
        let (client_end, server_end) = MockEnd::pair();
        let client = component_startup::Client::new(client_end);
        client.begin(LOG_CAP, 0, 0).expect("begin write");

        let started = receive_startup_transaction(&server_end)
            .expect("dispatch ok")
            .expect("transaction complete");

        assert_eq!(started.exports().count(), 0);
        assert_eq!(started.slots().count(), 0);
    }

    #[test]
    fn an_export_after_the_first_slot_breaks_the_transaction() {
        let (client_end, server_end) = MockEnd::pair();
        let client = component_startup::Client::new(client_end);
        client.begin(LOG_CAP, 2, 1).expect("begin write");
        client.export(SERVICE_CAP).expect("export write");
        client.slot(IMAGE_CAP).expect("slot write");
        client.export(ADMIN_CAP).expect("export write");

        assert_eq!(
            receive_startup_transaction(&server_end).expect("dispatch ok"),
            None
        );
    }

    #[test]
    fn an_export_past_the_promised_count_breaks_the_transaction() {
        let (client_end, server_end) = MockEnd::pair();
        let client = component_startup::Client::new(client_end);
        client.begin(LOG_CAP, 1, 1).expect("begin write");
        client.export(SERVICE_CAP).expect("export write");
        client.export(ADMIN_CAP).expect("export write");

        assert_eq!(
            receive_startup_transaction(&server_end).expect("dispatch ok"),
            None
        );
    }

    #[test]
    fn a_slot_before_the_last_export_breaks_the_transaction() {
        let (client_end, server_end) = MockEnd::pair();
        let client = component_startup::Client::new(client_end);
        client.begin(LOG_CAP, 1, 1).expect("begin write");
        client.slot(IMAGE_CAP).expect("slot write");

        assert_eq!(
            receive_startup_transaction(&server_end).expect("dispatch ok"),
            None
        );
    }

    #[test]
    fn a_slot_before_the_base_frame_breaks_the_transaction() {
        let (client_end, server_end) = MockEnd::pair();
        let client = component_startup::Client::new(client_end);
        client.slot(IMAGE_CAP).expect("slot write");

        assert_eq!(
            receive_startup_transaction(&server_end).expect("dispatch ok"),
            None
        );
    }

    #[test]
    fn startup_closes_all_caps_when_the_cast_is_not_delivered() {
        let (client_end, server_end) = MockEnd::pair();
        drop(server_end);
        let client = component_startup::Client::new(client_end);
        let (log, log_fate) = fake(0x31);

        assert_eq!(client.begin(log, 1, 1), Err(IpcError::PeerClosed));
        assert_eq!(log_fate.get(), Fate::Dropped);

        let (service, service_fate) = fake(0x34);
        assert_eq!(client.export(service), Err(IpcError::PeerClosed));
        assert_eq!(service_fate.get(), Fate::Dropped);

        let (image, image_fate) = fake(0x35);
        assert_eq!(client.slot(image), Err(IpcError::PeerClosed));
        assert_eq!(image_fate.get(), Fate::Dropped);
    }

    #[test]
    fn the_declared_set_arrives_in_the_order_of_the_declaration() {
        let (client_end, server_end) = MockEnd::pair();
        let client = component_startup::Client::new(client_end);
        client.begin(LOG_CAP, 2, 2).expect("begin write");
        client.export(SERVICE_CAP).expect("export write");
        client.export(ADMIN_CAP).expect("export write");
        client.slot(IMAGE_CAP).expect("slot write");
        client.slot(DEVICE_CAP).expect("slot write");

        let started = receive_startup_transaction(&server_end)
            .expect("dispatch ok")
            .expect("transaction complete");

        let exports: Vec<Cap<kind::Port>> = started
            .declared_exports(2)
            .expect("the set matches the declaration")
            .collect();
        assert_eq!(exports, [SERVICE_CAP, ADMIN_CAP]);
        let slots: Vec<Cap<kind::Any>> = started
            .declared_slots(2)
            .expect("the set matches the declaration")
            .collect();
        assert_eq!(slots, [IMAGE_CAP, DEVICE_CAP]);
        assert!(started.declared_exports(1).is_none());
        assert!(started.declared_slots(1).is_none());
    }

    #[test]
    fn a_transaction_torn_after_the_base_frame_reports_the_transport_failure() {
        let (client_end, server_end) = MockEnd::pair();
        let client = component_startup::Client::new(client_end);
        client.begin(LOG_CAP, 0, 1).expect("begin write");
        // Отправитель ушёл между базовым кадром и обещанным слотом.
        drop(client);

        assert_eq!(
            receive_startup_transaction(&server_end),
            Err(IpcError::PeerClosed)
        );
    }

    #[test]
    fn a_second_base_frame_breaks_the_transaction() {
        let (client_end, server_end) = MockEnd::pair();
        let client = component_startup::Client::new(client_end);
        client.begin(LOG_CAP, 0, 1).expect("begin write");
        client.begin(LOG_CAP, 0, 1).expect("begin write");

        assert_eq!(
            receive_startup_transaction(&server_end).expect("dispatch ok"),
            None
        );
    }

    #[test]
    fn a_session_request_before_the_end_of_the_transaction_is_refused() {
        let mut receiver = StartupReceiver::default();
        receiver.begin(LOG_CAP, 1, 0);

        assert_eq!(
            receiver.open_session(0, scope_of("")),
            Err(SessionError::Refused)
        );
    }

    #[test]
    fn open_session_carries_its_export_and_scope_and_may_refuse() {
        let (client_end, server_end) = MockEnd::pair();
        let mut component = Component::default();
        std::thread::scope(|scope| {
            let server = scope.spawn(|| {
                for _ in 0..3 {
                    server_end
                        .wait_readable(ipc::Timeout::INFINITE)
                        .expect("server wait");
                    component_startup::dispatch(&mut component, &server_end).expect("dispatch ok");
                }
            });

            let client = component_startup::Client::new(client_end);
            let mut session = client.open_session(0, scope_of("")).expect("call");
            assert_eq!(session.value, Ok(SESSION_CAP));
            assert_eq!(session.claim(SESSION_CAP), Some(SESSION_CAP));
            let mut scoped = client.open_session(0, scope_of(SCOPE)).expect("call");
            assert_eq!(scoped.value, Ok(SCOPED_CAP));
            assert_eq!(scoped.claim(SCOPED_CAP), Some(SCOPED_CAP));
            assert_eq!(
                client.open_session(1, scope_of(SCOPE)).expect("call").value,
                Err(SessionError::Refused)
            );
            server.join().expect("server joins");
        });

        assert_eq!(
            component.requests,
            [
                (0, String::new()),
                (0, SCOPE.to_string()),
                (1, SCOPE.to_string())
            ]
        );
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
