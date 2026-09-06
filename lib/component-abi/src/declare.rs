/// Кладёт запись манифеста компонента в секцию его программы.
///
/// Служебный вход. Программа объявляет манифест макросом
/// `component_startup::component_manifest!`, раскрытие которого вызывает этот.
///
/// Раскрытие порождает `static` с байтами записи из [`encode_manifest`](crate::encode_manifest).
/// Имя секции [`COMPONENT_MANIFEST_SECTION`](crate::COMPONENT_MANIFEST_SECTION) проставляется
/// сборкой под таргет системы, а на хосте `static` остаётся вне секции.
#[doc(hidden)]
#[macro_export]
macro_rules! __component_manifest_record {
    (
        export = [$($export:literal)?]
        control = [$($control:literal)?]
        establishes = [$($establishes:literal)?]
        slots { $($kind:ident $(($($parameter:tt)*))?),* }
    ) => {
        const _: () = {
            const SLOTS: &[$crate::SlotDeclaration] = &[$(
                $crate::__component_manifest_record!(@slot $kind $(($($parameter)*))?)
            ),*];
            const EXPORT: &str = $crate::__component_manifest_record!(@protocol $($export)?);
            const CONTROL: &str = $crate::__component_manifest_record!(@protocol $($control)?);
            const ESTABLISHES_SESSIONS: bool =
                $crate::__component_manifest_record!(@establishes $($establishes)?);
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
    (@protocol) => { "" };
    (@protocol $protocol:literal) => { $protocol };
    (@establishes) => { false };
    (@establishes $establishes:literal) => { $establishes };
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
    use crate::{
        COMPONENT_MANIFEST_SECTION, SlotKind, decode_manifest, encode_manifest, manifest_len,
    };

    const FILE_SYSTEM_SLOTS: &[crate::SlotDeclaration] = &[crate::SlotDeclaration::userland()];
    const FILE_SYSTEM_LEN: usize = manifest_len(FILE_SYSTEM_SLOTS, "", "");

    const NAMED_SLOTS: &[crate::SlotDeclaration] = &[crate::__component_manifest_record!(
        @slot service(protocol = "FileSystem", instance = "ramfs")
    )];
    const NAMED_LEN: usize = manifest_len(NAMED_SLOTS, "", "");

    const ADMIN_SLOTS: &[crate::SlotDeclaration] = &[crate::__component_manifest_record!(
        @slot control(protocol = "NamespaceAdmin", instance = "namespace-router")
    )];
    const ADMIN_LEN: usize = manifest_len(ADMIN_SLOTS, "Namespace", "NamespaceAdmin");

    const DEVICE_SLOTS: &[crate::SlotDeclaration] = &[
        crate::__component_manifest_record!(@slot irq(compatible = ["arm,pl031", "arm,primecell"])),
        crate::__component_manifest_record!(@slot mmio(compatible = ["arm,pl031"], resource = 1)),
    ];
    const DEVICE_LEN: usize = manifest_len(DEVICE_SLOTS, "", "");

    crate::__component_manifest_record! {
        export = ["FileSystem"]
        control = []
        establishes = [true]
        slots { userland }
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
    fn a_device_declaration_keeps_the_order_of_its_slots() {
        let bytes: [u8; DEVICE_LEN] = encode_manifest(DEVICE_SLOTS, "", "", false);

        let manifest = decode_manifest(&bytes).expect("record decodes");
        let kinds: std::vec::Vec<SlotKind> = manifest.slots().map(|slot| slot.kind()).collect();
        assert_eq!(kinds, [SlotKind::Irq, SlotKind::Mmio]);
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
