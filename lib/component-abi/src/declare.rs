/// Объявляет манифест компонента: обслуживаемые протоколы и именованные слоты
/// вместе с тем, чем композиция их наполняет - строками совместимости у
/// устройства и именем протокола у импорта. Порождает приёмник стартовой
/// транзакции с полями-капами и запись манифеста в секции программы.
///
/// Ключ `exports` называет протокол service-порта, ключ `controls` - протокол
/// control-порта: порты разные, и объявляются они порознь.
///
/// Ключ `establishes_sessions: true` объявляет, что сессии своего service-порта компонент
/// заводит сам: композиция просит их операцией `ComponentSessions::open_session` на его
/// control-порту вместо минта значка.
///
/// Импорт протокола принимает необязательное `instance` - имя экземпляра-провайдера.
/// Импорт без имени связывается с единственным провайдером протокола в композиции.
///
/// Слот-каталог называет `instance` обязательно: протокола у него нет, а сессию своего
/// каталога компонент берёт у названного провайдера.
///
/// ```ignore
/// component_abi::component_manifest! {
///     pub struct ImageFileSystemStartup {
///         exports: "FileSystem",
///         establishes_sessions: true,
///         slots: { image: userland },
///     }
/// }
///
/// component_abi::component_manifest! {
///     pub struct RouterStartup {
///         exports: "Namespace",
///         controls: "NamespaceAdmin",
///         slots: { image: userland },
///     }
/// }
///
/// component_abi::component_manifest! {
///     pub struct DemoStartup {
///         slots: {
///             files: service(protocol = "FileSystem", instance = "ramfs"),
///             mounts: control(protocol = "NamespaceAdmin"),
///             area: directory(instance = "filesystem-imagefs"),
///         },
///     }
/// }
/// ```
///
/// Форма без структуры кладёт только запись манифеста: приёмник транзакции программе на
/// стандартной библиотеке не нужен, набор ей отдаёт `std::os::molecular`.
///
/// ```ignore
/// component_abi::component_manifest! {
///     slots: { rtc: service(protocol = "Rtc") },
/// }
/// ```
#[macro_export]
macro_rules! component_manifest {
    (
        $(exports: $export:literal,)?
        $(controls: $control:literal,)?
        $(establishes_sessions: $establishes:literal,)?
        slots: { $($slot:ident: $kind:ident $(($($parameter:tt)*))?),* $(,)? } $(,)?
    ) => {
        $crate::component_manifest!(@section
            &[$($crate::component_manifest!(@slot $kind $(($($parameter)*))?)),*],
            $crate::component_manifest!(@protocol $($export)?),
            $crate::component_manifest!(@protocol $($control)?),
            $crate::component_manifest!(@establishes $($establishes)?)
        );
    };
    (
        $(#[$attribute:meta])*
        $visibility:vis struct $name:ident {
            $(exports: $export:literal,)?
            $(controls: $control:literal,)?
            $(establishes_sessions: $establishes:literal,)?
            slots: { $($slot:ident: $kind:ident $(($($parameter:tt)*))?),* $(,)? } $(,)?
        }
    ) => {
        $crate::component_manifest!(@declare
            $(#[$attribute])*
            $visibility struct $name
            export = [$($export)?]
            control = [$($control)?]
            establishes = [$($establishes)?]
            slots { $($slot: $kind $(($($parameter)*))?),* }
        );
    };
    (@declare
        $(#[$attribute:meta])*
        $visibility:vis struct $name:ident
        export = [$($export:literal)?]
        control = [$($control:literal)?]
        establishes = [$($establishes:literal)?]
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

        $crate::component_manifest!(@section
            &[$($crate::component_manifest!(@slot $kind $(($($parameter)*))?)),*],
            $crate::component_manifest!(@protocol $($export)?),
            $crate::component_manifest!(@protocol $($control)?),
            $crate::component_manifest!(@establishes $($establishes)?)
        );
    };
    (@protocol) => { "" };
    (@protocol $protocol:literal) => { $protocol };
    (@establishes) => { false };
    (@establishes $establishes:literal) => { $establishes };
    (@section $slots:expr, $export:expr, $control:expr, $establishes:expr) => {
        const _: () = {
            const SLOTS: &[$crate::SlotDeclaration] = $slots;
            const EXPORT: &str = $export;
            const CONTROL: &str = $control;
            const ESTABLISHES_SESSIONS: bool = $establishes;
            const RECORD_LEN: usize = $crate::manifest_len(SLOTS, EXPORT, CONTROL);

            // Секцию читает сборщик образа из ELF программы, и кладёт её только сборка под
            // таргет системы: на хосте образ не собирается, а имя секции там своё.
            #[cfg_attr(
                target_os = "molecular",
                unsafe(link_section = ".component.manifest")
            )]
            #[used]
            static MANIFEST: [u8; RECORD_LEN] =
                $crate::encode_manifest(SLOTS, EXPORT, CONTROL, ESTABLISHES_SESSIONS);
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
    (@slot service(protocol = $protocol:literal, instance = $instance:literal)) => {
        $crate::SlotDeclaration::service(&[$protocol, $instance])
    };
    (@slot control(protocol = $protocol:literal)) => {
        $crate::SlotDeclaration::control(&[$protocol])
    };
    (@slot control(protocol = $protocol:literal, instance = $instance:literal)) => {
        $crate::SlotDeclaration::control(&[$protocol, $instance])
    };
    (@slot directory(instance = $instance:literal)) => {
        $crate::SlotDeclaration::directory(&[$instance])
    };
    (@slot supervisor) => { $crate::SlotDeclaration::supervisor() };
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

    const FILE_SYSTEM_SLOTS: &[crate::SlotDeclaration] = &[crate::SlotDeclaration::userland()];
    const FILE_SYSTEM_LEN: usize = manifest_len(FILE_SYSTEM_SLOTS, "", "");

    const NAMED_SLOTS: &[crate::SlotDeclaration] = &[crate::component_manifest!(
        @slot service(protocol = "FileSystem", instance = "ramfs")
    )];
    const NAMED_LEN: usize = manifest_len(NAMED_SLOTS, "", "");

    const ADMIN_SLOTS: &[crate::SlotDeclaration] = &[crate::component_manifest!(
        @slot control(protocol = "NamespaceAdmin", instance = "namespace-router")
    )];
    const ADMIN_LEN: usize = manifest_len(ADMIN_SLOTS, "Namespace", "NamespaceAdmin");

    crate::component_manifest! {
        struct ImageFileSystemStartup {
            exports: "FileSystem",
            establishes_sessions: true,
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
            slots: { area: directory(instance = "filesystem-imagefs") },
        }
    }

    crate::component_manifest! {
        struct RouterStartup {
            exports: "Namespace",
            controls: "NamespaceAdmin",
            slots: { mounts: control(protocol = "NamespaceAdmin", instance = "namespace-router") },
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
            ImageFileSystemStartup::receive(&server_end)
                .expect("dispatch ok")
                .is_none()
        );
    }

    #[test]
    fn a_userland_declaration_takes_its_slot() {
        let (client_end, server_end) = MockEnd::pair();
        let sender = client(client_end);
        sender
            .start(LOG_CAP, CONTROL_CAP, SERVICE_CAP, 1)
            .expect("start write");
        sender.slot(IMAGE_CAP).expect("slot write");

        let started = ImageFileSystemStartup::receive(&server_end)
            .expect("dispatch ok")
            .expect("transaction matches the declaration");

        assert_eq!(started.log, LOG_CAP);
        assert_eq!(started.control, CONTROL_CAP);
        assert_eq!(started.service, SERVICE_CAP);
        assert_eq!(started.image, IMAGE_CAP);
    }

    #[test]
    fn a_named_service_slot_declares_its_protocol_and_instance() {
        let bytes: [u8; NAMED_LEN] = encode_manifest(NAMED_SLOTS, "", "", false);

        let manifest = decode_manifest(&bytes).expect("record decodes");
        let slot = manifest.slots().next().expect("service slot");
        assert_eq!(slot.kind(), SlotKind::Service);
        assert_eq!(
            slot.names().collect::<std::vec::Vec<_>>(),
            ["FileSystem", "ramfs"]
        );
    }

    #[test]
    fn a_control_slot_declares_its_protocol_beside_the_served_one() {
        let bytes: [u8; ADMIN_LEN] =
            encode_manifest(ADMIN_SLOTS, "Namespace", "NamespaceAdmin", false);

        let manifest = decode_manifest(&bytes).expect("record decodes");
        assert_eq!(manifest.export(), Some("Namespace"));
        assert_eq!(manifest.control(), Some("NamespaceAdmin"));
        let slot = manifest.slots().next().expect("control slot");
        assert_eq!(slot.kind(), SlotKind::Control);
        assert_eq!(
            slot.names().collect::<std::vec::Vec<_>>(),
            ["NamespaceAdmin", "namespace-router"]
        );
    }

    #[test]
    fn a_control_slot_arrives_as_a_named_capability() {
        let (client_end, server_end) = MockEnd::pair();
        let sender = client(client_end);
        sender
            .start(LOG_CAP, CONTROL_CAP, SERVICE_CAP, 1)
            .expect("start write");
        sender.slot(DEVICE_CAP).expect("slot write");

        let started = RouterStartup::receive(&server_end)
            .expect("dispatch ok")
            .expect("transaction matches the declaration");

        assert_eq!(started.log, LOG_CAP);
        assert_eq!(started.control, CONTROL_CAP);
        assert_eq!(started.service, SERVICE_CAP);
        assert_eq!(started.mounts, DEVICE_CAP);
    }

    #[test]
    fn a_declaration_encodes_its_slots_and_export() {
        // Имя секции макрос пишет литералом: `link_section` берёт только его.
        assert_eq!(COMPONENT_MANIFEST_SECTION, ".component.manifest");
        let bytes: [u8; FILE_SYSTEM_LEN] = encode_manifest(FILE_SYSTEM_SLOTS, "", "", false);

        let manifest = decode_manifest(&bytes).expect("record decodes");
        assert_eq!(manifest.export(), None);
        assert_eq!(manifest.control(), None);
        let slot = manifest.slots().next().expect("userland slot");
        assert_eq!(slot.kind(), SlotKind::Userland);
    }
}
