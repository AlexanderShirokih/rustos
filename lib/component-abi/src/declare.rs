/// Объявляет манифест компонента: экспортируемый протокол и именованные слоты
/// вместе с тем, чем композиция их наполняет - строками совместимости у
/// устройства и именем протокола у сервиса. Порождает приёмник стартовой
/// транзакции с полями-капами и запись манифеста в секции программы.
///
/// ```ignore
/// component_abi::component_manifest! {
///     pub struct RouterStartup {
///         exports: "Namespace",
///         slots: { image: userland },
///     }
/// }
///
/// component_abi::component_manifest! {
///     pub struct RtcStartup {
///         slots: {
///             line: irq(compatible = ["arm,pl031"]),
///             window: mmio(compatible = ["arm,pl031"]),
///         },
///     }
/// }
///
/// component_abi::component_manifest! {
///     pub struct DemoStartup {
///         slots: { area: directory },
///     }
/// }
/// ```
#[macro_export]
macro_rules! component_manifest {
    (
        $(#[$attribute:meta])*
        $visibility:vis struct $name:ident {
            exports: $export:literal,
            slots: { $($slot:ident: $kind:ident $(($($parameter:tt)*))?),* $(,)? } $(,)?
        }
    ) => {
        $crate::component_manifest!(@declare
            $(#[$attribute])*
            $visibility struct $name
            export = $export
            slots { $($slot: $kind $(($($parameter)*))?),* }
        );
    };
    (
        $(#[$attribute:meta])*
        $visibility:vis struct $name:ident {
            slots: { $($slot:ident: $kind:ident $(($($parameter:tt)*))?),* $(,)? } $(,)?
        }
    ) => {
        $crate::component_manifest!(@declare
            $(#[$attribute])*
            $visibility struct $name
            export = ""
            slots { $($slot: $kind $(($($parameter)*))?),* }
        );
    };
    (@declare
        $(#[$attribute:meta])*
        $visibility:vis struct $name:ident
        export = $export:literal
        slots { $($slot:ident: $kind:ident $(($($parameter:tt)*))?),* }
    ) => {
        $(#[$attribute])*
        $visibility struct $name {
            pub log: ::ipc::wire::Cap,
            pub control: ::ipc::wire::Cap,
            pub service: ::ipc::wire::Cap,
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
                    $($slot,)*
                }))
            }
        }

        const _: () = {
            const SLOTS: &[$crate::SlotDeclaration] = &[
                $($crate::component_manifest!(@slot $kind $(($($parameter)*))?)),*
            ];
            const RECORD_LEN: usize = $crate::manifest_len(SLOTS, $export);

            #[cfg_attr(target_os = "none", unsafe(link_section = ".component.manifest"))]
            #[used]
            static MANIFEST: [u8; RECORD_LEN] = $crate::encode_manifest(SLOTS, $export);
        };
    };
    (@slot userland) => { $crate::SlotDeclaration::userland() };
    (@slot irq(compatible = [$($compatible:literal),+ $(,)?])) => {
        $crate::SlotDeclaration::irq(&[$($compatible),+], 0)
    };
    (@slot irq(compatible = [$($compatible:literal),+ $(,)?], resource = $resource:literal)) => {
        $crate::SlotDeclaration::irq(&[$($compatible),+], $resource)
    };
    (@slot mmio(compatible = [$($compatible:literal),+ $(,)?])) => {
        $crate::SlotDeclaration::mmio(&[$($compatible),+], 0)
    };
    (@slot mmio(compatible = [$($compatible:literal),+ $(,)?], resource = $resource:literal)) => {
        $crate::SlotDeclaration::mmio(&[$($compatible),+], $resource)
    };
    (@slot service(protocol = $protocol:literal)) => {
        $crate::SlotDeclaration::service(&[$protocol])
    };
    (@slot directory) => { $crate::SlotDeclaration::directory() };
}

#[cfg(test)]
mod tests {
    use core::num::NonZeroU32;

    use ipc::wire::Cap;
    use ipc_test::MockEnd;

    use crate::{
        COMPONENT_MANIFEST_SECTION, SlotKind, component_startup, decode_manifest, encode_manifest,
        manifest_len,
    };

    const LOG_CAP: Cap = cap(0x21);
    const CONTROL_CAP: Cap = cap(0x23);
    const SERVICE_CAP: Cap = cap(0x24);
    const IMAGE_CAP: Cap = cap(0x25);
    const DEVICE_CAP: Cap = cap(0x26);

    const ROUTER_SLOTS: &[crate::SlotDeclaration] = &[crate::SlotDeclaration::userland()];
    const ROUTER_LEN: usize = manifest_len(ROUTER_SLOTS, "Namespace");

    crate::component_manifest! {
        struct RouterStartup {
            exports: "Namespace",
            slots: { image: userland },
        }
    }

    crate::component_manifest! {
        struct DeviceStartup {
            slots: {
                line: irq(compatible = ["arm,pl031", "arm,primecell"]),
                window: mmio(compatible = ["arm,pl031"], resource = 1),
            },
        }
    }

    crate::component_manifest! {
        struct ConsumerStartup {
            slots: { namespace: service(protocol = "Namespace") },
        }
    }

    crate::component_manifest! {
        struct AreaStartup {
            slots: { area: directory },
        }
    }

    const fn cap(raw: u32) -> Cap {
        Cap::from_raw(NonZeroU32::new(raw).expect("non-zero"))
    }

    fn client(end: MockEnd) -> component_startup::Client<MockEnd> {
        component_startup::Client::new(end)
    }

    #[test]
    fn a_declaration_names_every_slot_of_the_transaction() {
        let (client_end, server_end) = MockEnd::pair();
        let sender = client(client_end);
        sender
            .start(LOG_CAP, CONTROL_CAP, SERVICE_CAP, 2)
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
    fn a_service_slot_arrives_as_a_named_capability() {
        let (client_end, server_end) = MockEnd::pair();
        let sender = client(client_end);
        sender
            .start(LOG_CAP, CONTROL_CAP, SERVICE_CAP, 1)
            .expect("start write");
        sender.slot(DEVICE_CAP).expect("slot write");

        let started = ConsumerStartup::receive(&server_end)
            .expect("dispatch ok")
            .expect("transaction matches the declaration");

        assert_eq!(started.log, LOG_CAP);
        assert_eq!(started.control, CONTROL_CAP);
        assert_eq!(started.service, SERVICE_CAP);
        assert_eq!(started.namespace, DEVICE_CAP);
    }

    #[test]
    fn a_directory_slot_arrives_as_a_named_capability() {
        let (client_end, server_end) = MockEnd::pair();
        let sender = client(client_end);
        sender
            .start(LOG_CAP, CONTROL_CAP, SERVICE_CAP, 1)
            .expect("start write");
        sender.slot(IMAGE_CAP).expect("slot write");

        let started = AreaStartup::receive(&server_end)
            .expect("dispatch ok")
            .expect("transaction matches the declaration");

        assert_eq!(started.log, LOG_CAP);
        assert_eq!(started.control, CONTROL_CAP);
        assert_eq!(started.service, SERVICE_CAP);
        assert_eq!(started.area, IMAGE_CAP);
    }

    #[test]
    fn a_transaction_past_the_declared_slots_is_refused() {
        let (client_end, server_end) = MockEnd::pair();
        let sender = client(client_end);
        sender
            .start(LOG_CAP, CONTROL_CAP, SERVICE_CAP, 2)
            .expect("start write");
        sender.slot(DEVICE_CAP).expect("slot write");
        sender.slot(IMAGE_CAP).expect("slot write");

        assert!(
            ConsumerStartup::receive(&server_end)
                .expect("dispatch ok")
                .is_none()
        );
    }

    #[test]
    fn a_transaction_short_of_the_declared_slots_is_refused() {
        let (client_end, server_end) = MockEnd::pair();
        let sender = client(client_end);
        sender
            .start(LOG_CAP, CONTROL_CAP, SERVICE_CAP, 0)
            .expect("start write");

        assert!(
            RouterStartup::receive(&server_end)
                .expect("dispatch ok")
                .is_none()
        );
    }

    #[test]
    fn an_exporting_declaration_takes_its_slot() {
        let (client_end, server_end) = MockEnd::pair();
        let sender = client(client_end);
        sender
            .start(LOG_CAP, CONTROL_CAP, SERVICE_CAP, 1)
            .expect("start write");
        sender.slot(IMAGE_CAP).expect("slot write");

        let started = RouterStartup::receive(&server_end)
            .expect("dispatch ok")
            .expect("transaction matches the declaration");

        assert_eq!(started.log, LOG_CAP);
        assert_eq!(started.control, CONTROL_CAP);
        assert_eq!(started.service, SERVICE_CAP);
        assert_eq!(started.image, IMAGE_CAP);
    }

    #[test]
    fn a_declaration_encodes_its_slots_and_export() {
        assert_eq!(COMPONENT_MANIFEST_SECTION, ".component.manifest");
        let bytes: [u8; ROUTER_LEN] = encode_manifest(ROUTER_SLOTS, "Namespace");

        let manifest = decode_manifest(&bytes).expect("record decodes");
        assert_eq!(manifest.export(), Some("Namespace"));
        let slot = manifest.slots().next().expect("userland slot");
        assert_eq!(slot.kind(), SlotKind::Userland);
    }
}
