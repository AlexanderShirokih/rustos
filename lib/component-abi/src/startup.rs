use crate::config::{COMPONENT_CONFIG_MAX, ComponentConfig};

/// Одноразовый стартовый кадр компонента композиции.
#[ipc::protocol(name = "ComponentStartup")]
pub trait ComponentStartup {
    /// Отдаёт компоненту лог-сессию, регион userland-образа, серверные стороны
    /// его control- и service-портов и путь записи его конфигурации.
    #[cast]
    fn start(
        &self,
        log: ipc::wire::Cap,
        image: ipc::wire::Cap,
        control: ipc::wire::Cap,
        service: ipc::wire::Cap,
        config: ipc::wire::Str<{ COMPONENT_CONFIG_MAX }>,
    );
}

/// Capability и параметры, принятые из стартового кадра.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ComponentStartupFrame {
    /// Делегированная log-сессия.
    pub log: ipc::wire::Cap,
    /// Read-only userland image.
    pub image: ipc::wire::Cap,
    /// Серверная сторона control-порта: его клиентскую сторону держит supervisor.
    pub control: ipc::wire::Cap,
    /// Серверная сторона service-порта: его сессии supervisor минтит клиентам.
    pub service: ipc::wire::Cap,
    /// Запись образа с конфигурацией компонента.
    pub config: ComponentConfig,
}

impl ComponentStartupService for Option<ComponentStartupFrame> {
    fn start(
        &mut self,
        log: ipc::wire::Cap,
        image: ipc::wire::Cap,
        control: ipc::wire::Cap,
        service: ipc::wire::Cap,
        config: ipc::wire::Str<'_, { COMPONENT_CONFIG_MAX }>,
    ) {
        let Some(config) = ComponentConfig::new(config.as_str()) else {
            return;
        };
        *self = Some(ComponentStartupFrame {
            log,
            image,
            control,
            service,
            config,
        });
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
        COMPONENT_CONFIG_MAX, ComponentConfig, ComponentStartupClient, ComponentStartupFrame,
        dispatch_component_startup,
    };

    const LOG_CAP: Cap = cap(0x21);
    const IMAGE_CAP: Cap = cap(0x22);
    const CONTROL_CAP: Cap = cap(0x23);
    const SERVICE_CAP: Cap = cap(0x24);

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
    fn startup_carries_four_caps_and_the_config_record() {
        let (client_end, server_end) = MockEnd::pair();
        let client = ComponentStartupClient::new(client_end);
        client
            .start(LOG_CAP, IMAGE_CAP, CONTROL_CAP, SERVICE_CAP, config())
            .expect("start write");

        let mut sink = None;
        dispatch_component_startup(&mut sink, &server_end).expect("dispatch ok");
        assert_eq!(
            sink,
            Some(ComponentStartupFrame {
                log: LOG_CAP,
                image: IMAGE_CAP,
                control: CONTROL_CAP,
                service: SERVICE_CAP,
                config: ComponentConfig::new(CONFIG_RECORD).expect("record path"),
            })
        );
    }

    #[test]
    fn startup_without_a_config_record_is_not_accepted() {
        let (client_end, server_end) = MockEnd::pair();
        let client = ComponentStartupClient::new(client_end);
        client
            .start(
                LOG_CAP,
                IMAGE_CAP,
                CONTROL_CAP,
                SERVICE_CAP,
                Str::new("").expect("empty path fits the frame"),
            )
            .expect("start write");

        let mut sink = None;
        dispatch_component_startup(&mut sink, &server_end).expect("dispatch ok");
        assert_eq!(sink, None);
    }

    #[test]
    fn startup_closes_all_caps_when_the_cast_is_not_delivered() {
        let (client_end, server_end) = MockEnd::pair();
        drop(server_end);
        let client = ComponentStartupClient::new(client_end);
        let (log, log_fate) = fake(0x31);
        let (image, image_fate) = fake(0x32);
        let (control, control_fate) = fake(0x33);
        let (service, service_fate) = fake(0x34);

        assert_eq!(
            client.start(log, image, control, service, config()),
            Err(IpcError::PeerClosed)
        );
        assert_eq!(
            [
                log_fate.get(),
                image_fate.get(),
                control_fate.get(),
                service_fate.get(),
            ],
            [Fate::Dropped; 4]
        );
    }
}
