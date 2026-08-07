/// Объявляет манифест компонента: именованные слоты с их видами и запись
/// конфигурации. Порождает приёмник стартовой транзакции с полями-капами и
/// запись манифеста в секции программы.
///
/// ```ignore
/// component_abi::component_manifest! {
///     pub struct RouterStartup {
///         config,
///         slots: { image: image },
///     }
/// }
/// ```
#[macro_export]
macro_rules! component_manifest {
    (
        $(#[$attribute:meta])*
        $visibility:vis struct $name:ident {
            config,
            slots: { $($slot:ident: $kind:ident),* $(,)? } $(,)?
        }
    ) => {
        $crate::component_manifest!(@declare
            $(#[$attribute])*
            $visibility struct $name
            config = true (config)
            slots { $($slot: $kind),* }
        );
    };
    (
        $(#[$attribute:meta])*
        $visibility:vis struct $name:ident {
            slots: { $($slot:ident: $kind:ident),* $(,)? } $(,)?
        }
    ) => {
        $crate::component_manifest!(@declare
            $(#[$attribute])*
            $visibility struct $name
            config = false ()
            slots { $($slot: $kind),* }
        );
    };
    (@declare
        $(#[$attribute:meta])*
        $visibility:vis struct $name:ident
        config = $config:literal ($($config_field:ident)?)
        slots { $($slot:ident: $kind:ident),* }
    ) => {
        $(#[$attribute])*
        $visibility struct $name {
            pub log: ::ipc::wire::Cap,
            pub control: ::ipc::wire::Cap,
            pub service: ::ipc::wire::Cap,
            $(pub $config_field: $crate::ComponentConfig,)?
            $(pub $slot: ::ipc::wire::Cap,)*
        }

        impl $name {
            $visibility fn receive<T: ::ipc::Transport>(
                transport: &T,
            ) -> ::core::result::Result<::core::option::Option<Self>, ::ipc::wire::IpcError> {
                let ::core::option::Option::Some(started) =
                    $crate::receive_startup_transaction(transport)?
                else {
                    return ::core::result::Result::Ok(::core::option::Option::None);
                };
                if started.config.is_some() != $config {
                    return ::core::result::Result::Ok(::core::option::Option::None);
                }
                $(
                    let ::core::option::Option::Some($config_field) = started.config else {
                        return ::core::result::Result::Ok(::core::option::Option::None);
                    };
                )?
                let mut slots = started.slots();
                $(
                    let ::core::option::Option::Some($slot) = slots.next() else {
                        return ::core::result::Result::Ok(::core::option::Option::None);
                    };
                )*
                if slots.next().is_some() {
                    return ::core::result::Result::Ok(::core::option::Option::None);
                }
                ::core::result::Result::Ok(::core::option::Option::Some(Self {
                    log: started.log,
                    control: started.control,
                    service: started.service,
                    $($config_field,)?
                    $($slot,)*
                }))
            }
        }

        const _: () = {
            const SLOTS: &[$crate::SlotKind] =
                &[$($crate::component_manifest!(@kind $kind)),*];
            const RECORD_LEN: usize = $crate::COMPONENT_MANIFEST_HEADER_SIZE + SLOTS.len();

            #[cfg_attr(target_os = "none", unsafe(link_section = ".component.manifest"))]
            #[used]
            static MANIFEST: [u8; RECORD_LEN] = $crate::encode_manifest(SLOTS, $config);
        };
    };
    (@kind image) => { $crate::SlotKind::Image };
    (@kind irq) => { $crate::SlotKind::Irq };
    (@kind mmio) => { $crate::SlotKind::Mmio };
    (@kind service) => { $crate::SlotKind::Service };
}

#[cfg(test)]
mod tests {
    use core::num::NonZeroU32;

    use ipc::wire::{Cap, Str};
    use ipc_test::MockEnd;

    use crate::{COMPONENT_CONFIG_MAX, ComponentConfig, ComponentStartupClient};

    const LOG_CAP: Cap = cap(0x21);
    const CONTROL_CAP: Cap = cap(0x23);
    const SERVICE_CAP: Cap = cap(0x24);
    const IMAGE_CAP: Cap = cap(0x25);
    const DEVICE_CAP: Cap = cap(0x26);

    const CONFIG_RECORD: &str = "/boot/namespace.mounts";

    crate::component_manifest! {
        struct RouterStartup {
            config,
            slots: { image: image },
        }
    }

    crate::component_manifest! {
        struct DeviceStartup {
            slots: { line: irq, window: mmio },
        }
    }

    const fn cap(raw: u32) -> Cap {
        Cap::from_raw(NonZeroU32::new(raw).expect("non-zero"))
    }

    fn config(record: &str) -> Str<'_, COMPONENT_CONFIG_MAX> {
        Str::new(record).expect("record path fits the frame")
    }

    fn client(end: MockEnd) -> ComponentStartupClient<MockEnd> {
        ComponentStartupClient::new(end)
    }

    #[test]
    fn a_declaration_names_every_slot_of_the_transaction() {
        let (client_end, server_end) = MockEnd::pair();
        let sender = client(client_end);
        sender
            .start(LOG_CAP, CONTROL_CAP, SERVICE_CAP, config(""), 2)
            .expect("start write");
        sender.slot(DEVICE_CAP).expect("slot write");
        sender.slot(IMAGE_CAP).expect("slot write");

        let started = DeviceStartup::receive(&server_end)
            .expect("dispatch ok")
            .expect("transaction matches the declaration");

        assert_eq!(started.log, LOG_CAP);
        assert_eq!(started.control, CONTROL_CAP);
        assert_eq!(started.service, SERVICE_CAP);
        assert_eq!(started.line, DEVICE_CAP);
        assert_eq!(started.window, IMAGE_CAP);
    }

    #[test]
    fn a_declared_config_arrives_with_the_base_frame() {
        let (client_end, server_end) = MockEnd::pair();
        let sender = client(client_end);
        sender
            .start(LOG_CAP, CONTROL_CAP, SERVICE_CAP, config(CONFIG_RECORD), 1)
            .expect("start write");
        sender.slot(IMAGE_CAP).expect("slot write");

        let started = RouterStartup::receive(&server_end)
            .expect("dispatch ok")
            .expect("transaction matches the declaration");

        assert_eq!(started.log, LOG_CAP);
        assert_eq!(started.control, CONTROL_CAP);
        assert_eq!(started.service, SERVICE_CAP);
        assert_eq!(started.image, IMAGE_CAP);
        assert_eq!(
            started.config,
            ComponentConfig::new(CONFIG_RECORD).expect("record")
        );
    }

    #[test]
    fn a_transaction_past_the_declared_slots_is_refused() {
        let (client_end, server_end) = MockEnd::pair();
        let sender = client(client_end);
        sender
            .start(LOG_CAP, CONTROL_CAP, SERVICE_CAP, config(""), 2)
            .expect("start write");
        sender.slot(DEVICE_CAP).expect("slot write");
        sender.slot(IMAGE_CAP).expect("slot write");

        assert!(
            RouterStartup::receive(&server_end)
                .expect("dispatch ok")
                .is_none()
        );
    }

    #[test]
    fn a_transaction_without_the_declared_config_is_refused() {
        let (client_end, server_end) = MockEnd::pair();
        let sender = client(client_end);
        sender
            .start(LOG_CAP, CONTROL_CAP, SERVICE_CAP, config(""), 1)
            .expect("start write");
        sender.slot(IMAGE_CAP).expect("slot write");

        assert!(
            RouterStartup::receive(&server_end)
                .expect("dispatch ok")
                .is_none()
        );
    }

    #[test]
    fn a_config_the_declaration_does_not_name_is_refused() {
        let (client_end, server_end) = MockEnd::pair();
        let sender = client(client_end);
        sender
            .start(LOG_CAP, CONTROL_CAP, SERVICE_CAP, config(CONFIG_RECORD), 2)
            .expect("start write");
        sender.slot(DEVICE_CAP).expect("slot write");
        sender.slot(IMAGE_CAP).expect("slot write");

        assert!(
            DeviceStartup::receive(&server_end)
                .expect("dispatch ok")
                .is_none()
        );
    }
}
