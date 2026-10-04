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
        exports = [$($export:path $(=> [$($filter:tt)*])?),*]
        requirements { $($requirement:ident: $kind:ident $(($($parameter:tt)*))?),* }
    ) => {
        const _: () = {
            const REQUIREMENTS: &[$crate::RequirementDeclaration] = &[$(
                $crate::__component_manifest_record!(@requirement $requirement $kind $(($($parameter)*))?)
            ),*];
            const EXPORTS: &[$crate::ExportDeclaration] = &[$(
                $crate::__component_manifest_record!(@export $export $(=> [$($filter)*])?)
            ),*];
            const RECORD_LEN: usize = $crate::manifest_len(REQUIREMENTS, EXPORTS);

            #[cfg_attr(
                target_os = "molecular",
                unsafe(link_section = ".component.manifest")
            )]
            #[used]
            static MANIFEST: [u8; RECORD_LEN] = $crate::encode_manifest(REQUIREMENTS, EXPORTS);
        };
    };
    (@export $protocol:path) => {
        $crate::ExportDeclaration::new(<$protocol as $crate::__Protocol>::ID)
    };
    (@export $protocol:path => [$({ $($key:tt = $value:literal),+ $(,)? }),* $(,)?]) => {
        $crate::ExportDeclaration::filtered(<$protocol as $crate::__Protocol>::ID, &[$(&[$(
            $crate::Attribute {
                key: $crate::__component_manifest_record!(@key $key),
                value: $value.as_bytes(),
            }
        ),+]),*])
    };
    (@key $key:ident) => {
        stringify!($key).as_bytes()
    };
    (@key $key:literal) => {
        $key.as_bytes()
    };
    (@requirement log $($declaration:tt)*) => {
        compile_error!("`log` is the name reserved for the log its parent serves")
    };
    (@requirement $name:ident session($protocol:path)) => {
        $crate::RequirementDeclaration::session(stringify!($name), <$protocol as $crate::__Protocol>::ID)
    };
    (@requirement $name:ident all($protocol:path)) => {
        $crate::RequirementDeclaration::all(stringify!($name), <$protocol as $crate::__Protocol>::ID)
    };
}

#[cfg(test)]
mod tests {
    use crate::{
        COMPONENT_MANIFEST_SECTION, RequirementMode, decode_manifest, encode_manifest, manifest_len,
    };

    const NO_EXPORTS: &[crate::ExportDeclaration] = &[];

    ipc::protocol! {
        mod file_system {
            name = "FileSystem";
        }
    }

    ipc::protocol! {
        mod file_system_mount {
            name = "FileSystemMount";
        }
    }

    const TYPED_REQUIREMENTS: &[crate::RequirementDeclaration] =
        &[crate::__component_manifest_record!(
            @requirement files session(file_system::Protocol)
        )];
    const TYPED_LEN: usize = manifest_len(TYPED_REQUIREMENTS, NO_EXPORTS);

    const LITERAL_REQUIREMENTS: &[crate::RequirementDeclaration] =
        &[crate::RequirementDeclaration::session(
            "files",
            "FileSystem",
        )];

    const TYPED_EXPORTS: &[crate::ExportDeclaration] = &[
        crate::__component_manifest_record!(@export file_system::Protocol),
        crate::__component_manifest_record!(@export file_system_mount::Protocol => [
            { compatible = "a", "vendor,role" = "" },
            { compatible = "b" },
        ]),
    ];
    const EXPORTS_LEN: usize = manifest_len(&[], TYPED_EXPORTS);

    const REQUIREMENTS: &[crate::RequirementDeclaration] = &[
        crate::__component_manifest_record!(@requirement admin session(file_system_mount::Protocol)),
        crate::__component_manifest_record!(@requirement files session(file_system::Protocol)),
        crate::__component_manifest_record!(@requirement mounts all(file_system::Protocol)),
    ];
    const REQUIREMENTS_LEN: usize = manifest_len(REQUIREMENTS, NO_EXPORTS);

    crate::__component_manifest_record! {
        exports = [file_system::Protocol => [{ compatible = "a" }], file_system_mount::Protocol]
        requirements { files: all(file_system::Protocol), admin: session(file_system_mount::Protocol) }
    }

    #[test]
    fn a_session_requirement_carries_the_id_of_its_protocol() {
        let typed: [u8; TYPED_LEN] = encode_manifest(TYPED_REQUIREMENTS, NO_EXPORTS);
        let literal: [u8; TYPED_LEN] = encode_manifest(LITERAL_REQUIREMENTS, NO_EXPORTS);

        assert_eq!(typed, literal);
        let manifest = decode_manifest(&typed).expect("record decodes");
        let requirement = manifest.requirements().next().expect("session requirement");
        assert_eq!(requirement.mode(), RequirementMode::One);
        assert_eq!(
            requirement.protocol(),
            <file_system::Protocol as ipc::Protocol>::ID
        );
    }

    #[test]
    fn a_declaration_names_an_export_by_the_id_of_its_protocol() {
        let bytes: [u8; EXPORTS_LEN] = encode_manifest(&[], TYPED_EXPORTS);

        let manifest = decode_manifest(&bytes).expect("record decodes");
        let exports: std::vec::Vec<_> = manifest
            .exports()
            .map(|export| {
                let filters: std::vec::Vec<std::vec::Vec<_>> = export
                    .filters()
                    .map(|filter| filter.iter().map(|pair| (pair.key, pair.value)).collect())
                    .collect();
                (export.protocol(), filters)
            })
            .collect();

        let alternatives: std::vec::Vec<std::vec::Vec<(&[u8], &[u8])>> = std::vec![
            std::vec![(b"compatible", b"a"), (b"vendor,role", b"")],
            std::vec![(b"compatible", b"b")],
        ];
        assert_eq!(
            exports,
            [
                (
                    <file_system::Protocol as ipc::Protocol>::ID,
                    std::vec::Vec::new()
                ),
                (
                    <file_system_mount::Protocol as ipc::Protocol>::ID,
                    alternatives
                ),
            ]
        );
    }

    #[test]
    fn a_declaration_names_every_requirement_by_its_field_in_the_order_of_the_declaration() {
        let bytes: [u8; REQUIREMENTS_LEN] = encode_manifest(REQUIREMENTS, NO_EXPORTS);

        let manifest = decode_manifest(&bytes).expect("record decodes");
        let requirements: std::vec::Vec<_> = manifest
            .requirements()
            .map(|requirement| (requirement.name(), requirement.mode()))
            .collect();
        assert_eq!(
            requirements,
            [
                ("admin", RequirementMode::One),
                ("files", RequirementMode::One),
                ("mounts", RequirementMode::All)
            ]
        );
    }

    #[test]
    fn the_declared_section_is_the_manifest_section() {
        assert_eq!(COMPONENT_MANIFEST_SECTION, ".component.manifest");
    }
}
