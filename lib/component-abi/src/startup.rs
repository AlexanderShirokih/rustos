use ipc::{Transport, wire::IpcError};

use crate::config::{COMPONENT_CONFIG_MAX, ComponentConfig};

/// Граница числа слотов capability одного компонента.
pub const COMPONENT_SLOT_MAX: usize = 8;

/// Одноразовая стартовая транзакция компонента композиции.
#[ipc::protocol(name = "ComponentStartup")]
pub trait ComponentStartup {
    /// Открывает транзакцию: лог-сессия, серверные стороны control- и
    /// service-портов компонента, путь записи его конфигурации и число слотов,
    /// которые придут следом.
    #[cast]
    fn start(
        &self,
        log: ipc::wire::Cap,
        control: ipc::wire::Cap,
        service: ipc::wire::Cap,
        config: ipc::wire::Str<{ COMPONENT_CONFIG_MAX }>,
        slot_count: u16,
    );

    /// Отдаёт capability очередного слота в порядке плана.
    #[cast]
    fn slot(&self, capability: ipc::wire::Cap);
}

/// Capability и параметры принятой стартовой транзакции. Семантику позиции
/// слота знает сам компонент.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StartedComponent {
    /// Делегированная log-сессия.
    pub log: ipc::wire::Cap,
    /// Серверная сторона control-порта: его клиентскую сторону держит supervisor.
    pub control: ipc::wire::Cap,
    /// Серверная сторона service-порта: его сессии supervisor минтит клиентам.
    pub service: ipc::wire::Cap,
    /// Запись образа с конфигурацией компонента.
    pub config: Option<ComponentConfig>,
    slots: [Option<ipc::wire::Cap>; COMPONENT_SLOT_MAX],
    slot_count: u16,
}

/// Приёмник стартовой транзакции: базовый кадр и ровно `slot_count` слотов.
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
        dispatch_component_startup(&mut receiver, transport)?;
        if receiver.broken {
            return Ok(None);
        }
    }
    Ok(receiver.started)
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
}

impl StartupReceiver {
    const fn is_complete(&self) -> bool {
        match &self.started {
            Some(started) => self.received == started.slot_count,
            None => false,
        }
    }
}

impl ComponentStartupService for StartupReceiver {
    fn start(
        &mut self,
        log: ipc::wire::Cap,
        control: ipc::wire::Cap,
        service: ipc::wire::Cap,
        config: ipc::wire::Str<'_, { COMPONENT_CONFIG_MAX }>,
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
            config: ComponentConfig::new(config.as_str()),
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
        wire::{Cap, IpcError, Str},
    };
    use ipc_test::MockEnd;

    use super::{
        COMPONENT_CONFIG_MAX, ComponentConfig, ComponentStartupClient, receive_startup_transaction,
    };

    const LOG_CAP: Cap = cap(0x21);
    const CONTROL_CAP: Cap = cap(0x23);
    const SERVICE_CAP: Cap = cap(0x24);
    const IMAGE_CAP: Cap = cap(0x25);
    const DEVICE_CAP: Cap = cap(0x26);

    const CONFIG_RECORD: &str = "/boot/namespace.mounts";

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

    fn config() -> Str<'static, COMPONENT_CONFIG_MAX> {
        Str::new(CONFIG_RECORD).expect("record path fits the frame")
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
        let client = ComponentStartupClient::new(client_end);
        client
            .start(LOG_CAP, CONTROL_CAP, SERVICE_CAP, config(), 2)
            .expect("start write");
        client.slot(IMAGE_CAP).expect("slot write");
        client.slot(DEVICE_CAP).expect("slot write");

        let started = receive_startup_transaction(&server_end)
            .expect("dispatch ok")
            .expect("transaction complete");

        assert_eq!(started.log, LOG_CAP);
        assert_eq!(started.control, CONTROL_CAP);
        assert_eq!(started.service, SERVICE_CAP);
        assert_eq!(started.config, ComponentConfig::new(CONFIG_RECORD));
        assert_eq!(started.slots().count(), 2);
        assert_eq!(started.slot(0), Some(IMAGE_CAP));
        assert_eq!(started.slot(1), Some(DEVICE_CAP));
        assert_eq!(started.slot(2), None);
    }

    #[test]
    fn a_transaction_without_slots_needs_no_further_casts() {
        let (client_end, server_end) = MockEnd::pair();
        let client = ComponentStartupClient::new(client_end);
        client
            .start(
                LOG_CAP,
                CONTROL_CAP,
                SERVICE_CAP,
                Str::new("").expect("empty path fits the frame"),
                0,
            )
            .expect("start write");

        let started = receive_startup_transaction(&server_end)
            .expect("dispatch ok")
            .expect("transaction complete");

        assert_eq!(started.config, None);
        assert_eq!(started.slots().count(), 0);
    }

    #[test]
    fn a_slot_before_the_base_frame_breaks_the_transaction() {
        let (client_end, server_end) = MockEnd::pair();
        let client = ComponentStartupClient::new(client_end);
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
        let client = ComponentStartupClient::new(client_end);
        let (log, log_fate) = fake(0x31);
        let (control, control_fate) = fake(0x33);
        let (service, service_fate) = fake(0x34);

        assert_eq!(
            client.start(log, control, service, config(), 1),
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
}
