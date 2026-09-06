//! Стартовая транзакция компонента и её приём.
//!
//! Контракт [`component_startup`] переносит стартовый набор capability базовым кадром и
//! кадром на каждый слот. Принятая транзакция собирается в [`StartedComponent`], а до конца
//! транзакции приём доводит [`receive_startup_transaction`].

use core::num::NonZeroU64;

use ipc::{Transport, wire::IpcError};

/// Граница числа слотов capability одного компонента.
pub const COMPONENT_SLOT_MAX: usize = 8;

/// Значок клиентской capability control-порта, оставшейся у supervisor.
pub const CONTROL_LEASE_BADGE: NonZeroU64 = NonZeroU64::new(1).expect("lease badge is non-zero");

ipc::protocol! {
    /// Одноразовая стартовая транзакция компонента композиции: передача стартового
    /// набора capability.
    pub mod component_startup {
        name = "ComponentStartup";

        /// Открывает транзакцию: лог-сессия, серверные стороны control- и
        /// service-портов компонента и число слотов, которые придут следом.
        ///
        /// Каждый параметр-`Cap` передаёт владеющий хэндл. `slot_count` сверх
        /// [`COMPONENT_SLOT_MAX`](crate::COMPONENT_SLOT_MAX) и второй базовый кадр ломают
        /// транзакцию.
        cast fn start(log: Cap, control: Cap, service: Cap, slot_count: u16);

        /// Отдаёт capability очередного слота в порядке плана.
        ///
        /// Слот до базового кадра и слот сверх обещанного `slot_count` ломают транзакцию.
        cast fn slot(capability: Cap);
    }
}

/// Capability и параметры принятой стартовой транзакции. Семантику позиции
/// слота определяет сам компонент.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StartedComponent {
    /// Делегированная log-сессия.
    pub log: ipc::wire::Cap,
    /// Серверная сторона control-порта. Его клиентскую сторону держит supervisor.
    pub control: ipc::wire::Cap,
    /// Серверная сторона service-порта. Сессии этого порта выдаются клиентам помимо
    /// компонента, пока манифест не объявил `establishes_sessions`.
    pub service: ipc::wire::Cap,
    slots: [Option<ipc::wire::Cap>; COMPONENT_SLOT_MAX],
    slot_count: u16,
}

/// Приёмник стартовой транзакции: базовый кадр и ровно `slot_count` слотов.
///
/// Приёмник реализует `component_startup::Service`, и нарушенный порядок кадров даёт `None`
/// из [`receive_startup_transaction`].
#[derive(Debug, Default)]
pub struct StartupReceiver {
    started: Option<StartedComponent>,
    received: u16,
    broken: bool,
}

/// Принимает стартовую транзакцию целиком: базовый кадр и следом слоты в
/// порядке плана. `None` - транзакция не сложилась в контракт.
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

/// Уход значка `badge` на control-порту компонента - уход supervisor, и это событие
/// терминально. Прочие значки порта выданы импортом его control-протокола, и после их ухода
/// цикл приёма продолжается.
#[must_use]
pub const fn supervisor_gone(badge: u64) -> bool {
    badge == CONTROL_LEASE_BADGE.get()
}

impl StartedComponent {
    fn slot(&self, index: u16) -> Option<ipc::wire::Cap> {
        if index >= self.slot_count {
            return None;
        }
        *self.slots.get(usize::from(index))?
    }

    /// Capability слотов в порядке плана.
    pub fn slots(&self) -> impl Iterator<Item = ipc::wire::Cap> {
        let started = *self;
        (0..started.slot_count).map_while(move |index| started.slot(index))
    }

    /// Слоты транзакции в порядке плана для манифеста, объявившего `declared` слотов.
    /// Другое число слотов транзакции даёт `None`.
    pub fn declared_slots(&self, declared: u16) -> Option<impl Iterator<Item = ipc::wire::Cap>> {
        (self.slot_count == declared).then(|| self.slots())
    }
}

impl StartupReceiver {
    const fn is_complete(&self) -> bool {
        match &self.started {
            Some(started) => self.received == started.slot_count,
            None => false,
        }
    }
}

impl component_startup::Service for StartupReceiver {
    fn start(
        &mut self,
        log: ipc::wire::Cap,
        control: ipc::wire::Cap,
        service: ipc::wire::Cap,
        slot_count: u16,
    ) {
        if self.started.is_some() || usize::from(slot_count) > COMPONENT_SLOT_MAX {
            self.broken = true;
            return;
        }
        self.started = Some(StartedComponent {
            log,
            control,
            service,
            slots: [None; COMPONENT_SLOT_MAX],
            slot_count,
        });
    }

    fn slot(&mut self, capability: ipc::wire::Cap) {
        let Some(started) = self.started.as_mut() else {
            self.broken = true;
            return;
        };
        if self.received >= started.slot_count {
            self.broken = true;
            return;
        }
        let Some(place) = started.slots.get_mut(usize::from(self.received)) else {
            self.broken = true;
            return;
        };
        *place = Some(capability);
        self.received += 1;
    }
}

#[cfg(test)]
mod tests {
    use core::num::NonZeroU32;
    use std::{cell::Cell, rc::Rc};

    use ipc::{
        IntoWireHandle,
        wire::{Cap, IpcError},
    };
    use ipc_test::MockEnd;

    use super::{
        CONTROL_LEASE_BADGE, component_startup, receive_startup_transaction, supervisor_gone,
    };

    const LOG_CAP: Cap = cap(0x21);
    const CONTROL_CAP: Cap = cap(0x23);
    const SERVICE_CAP: Cap = cap(0x24);
    const IMAGE_CAP: Cap = cap(0x25);
    const DEVICE_CAP: Cap = cap(0x26);

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum Fate {
        Pending,
        Relinquished,
        Dropped,
    }

    struct FakeHandle {
        id: u32,
        fate: Rc<Cell<Fate>>,
    }

    impl IntoWireHandle for FakeHandle {
        fn wire_id(&self) -> u32 {
            self.id
        }

        fn relinquish(self) {
            self.fate.set(Fate::Relinquished);
            core::mem::forget(self);
        }
    }

    impl Drop for FakeHandle {
        fn drop(&mut self) {
            self.fate.set(Fate::Dropped);
        }
    }

    const fn cap(raw: u32) -> Cap {
        Cap::from_raw(NonZeroU32::new(raw).expect("non-zero"))
    }

    fn fake(id: u32) -> (FakeHandle, Rc<Cell<Fate>>) {
        let fate = Rc::new(Cell::new(Fate::Pending));
        (
            FakeHandle {
                id,
                fate: Rc::clone(&fate),
            },
            fate,
        )
    }

    #[test]
    fn a_transaction_carries_the_base_frame_and_every_slot() {
        let (client_end, server_end) = MockEnd::pair();
        let client = component_startup::Client::new(client_end);
        client
            .start(LOG_CAP, CONTROL_CAP, SERVICE_CAP, 2)
            .expect("start write");
        client.slot(IMAGE_CAP).expect("slot write");
        client.slot(DEVICE_CAP).expect("slot write");

        let started = receive_startup_transaction(&server_end)
            .expect("dispatch ok")
            .expect("transaction complete");

        assert_eq!(started.log, LOG_CAP);
        assert_eq!(started.control, CONTROL_CAP);
        assert_eq!(started.service, SERVICE_CAP);
        assert_eq!(started.slots().count(), 2);
        assert_eq!(started.slot(0), Some(IMAGE_CAP));
        assert_eq!(started.slot(1), Some(DEVICE_CAP));
        assert_eq!(started.slot(2), None);
    }

    #[test]
    fn a_transaction_without_slots_needs_no_further_casts() {
        let (client_end, server_end) = MockEnd::pair();
        let client = component_startup::Client::new(client_end);
        client
            .start(LOG_CAP, CONTROL_CAP, SERVICE_CAP, 0)
            .expect("start write");

        let started = receive_startup_transaction(&server_end)
            .expect("dispatch ok")
            .expect("transaction complete");

        assert_eq!(started.slots().count(), 0);
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
        let (control, control_fate) = fake(0x33);
        let (service, service_fate) = fake(0x34);

        assert_eq!(
            client.start(log, control, service, 1),
            Err(IpcError::PeerClosed)
        );
        assert_eq!(
            [log_fate.get(), control_fate.get(), service_fate.get()],
            [Fate::Dropped; 3]
        );

        let (image, image_fate) = fake(0x35);
        assert_eq!(client.slot(image), Err(IpcError::PeerClosed));
        assert_eq!(image_fate.get(), Fate::Dropped);
    }

    #[test]
    fn the_declared_slots_arrive_in_the_order_of_the_plan() {
        let (client_end, server_end) = MockEnd::pair();
        let client = component_startup::Client::new(client_end);
        client
            .start(LOG_CAP, CONTROL_CAP, SERVICE_CAP, 2)
            .expect("start write");
        client.slot(IMAGE_CAP).expect("slot write");
        client.slot(DEVICE_CAP).expect("slot write");

        let started = receive_startup_transaction(&server_end)
            .expect("dispatch ok")
            .expect("transaction complete");

        let slots: std::vec::Vec<Cap> = started
            .declared_slots(2)
            .expect("the set matches the declaration")
            .collect();
        assert_eq!(slots, [IMAGE_CAP, DEVICE_CAP]);
    }

    #[test]
    fn a_transaction_short_of_the_declared_slots_is_refused() {
        let (client_end, server_end) = MockEnd::pair();
        let client = component_startup::Client::new(client_end);
        client
            .start(LOG_CAP, CONTROL_CAP, SERVICE_CAP, 1)
            .expect("start write");
        client.slot(IMAGE_CAP).expect("slot write");

        let started = receive_startup_transaction(&server_end)
            .expect("dispatch ok")
            .expect("transaction complete");

        assert!(started.declared_slots(2).is_none());
    }

    #[test]
    fn a_transaction_past_the_declared_slots_is_refused() {
        let (client_end, server_end) = MockEnd::pair();
        let client = component_startup::Client::new(client_end);
        client
            .start(LOG_CAP, CONTROL_CAP, SERVICE_CAP, 2)
            .expect("start write");
        client.slot(IMAGE_CAP).expect("slot write");
        client.slot(DEVICE_CAP).expect("slot write");

        let started = receive_startup_transaction(&server_end)
            .expect("dispatch ok")
            .expect("transaction complete");

        assert!(started.declared_slots(1).is_none());
    }

    #[test]
    fn a_transaction_torn_after_the_base_frame_reports_the_transport_failure() {
        let (client_end, server_end) = MockEnd::pair();
        let client = component_startup::Client::new(client_end);
        client
            .start(LOG_CAP, CONTROL_CAP, SERVICE_CAP, 1)
            .expect("start write");
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
        client
            .start(LOG_CAP, CONTROL_CAP, SERVICE_CAP, 1)
            .expect("start write");
        client
            .start(LOG_CAP, CONTROL_CAP, SERVICE_CAP, 1)
            .expect("start write");

        assert_eq!(
            receive_startup_transaction(&server_end).expect("dispatch ok"),
            None
        );
    }

    #[test]
    fn only_the_lease_badge_of_the_control_port_reports_the_supervisor() {
        assert!(supervisor_gone(CONTROL_LEASE_BADGE.get()));
        // Значок потребителя control-протокола цикл приёма провайдера не завершает.
        assert!(!supervisor_gone(CONTROL_LEASE_BADGE.get() + 1));
        assert!(!supervisor_gone(0));
    }
}
