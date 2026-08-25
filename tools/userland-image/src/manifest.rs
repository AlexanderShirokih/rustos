use component_abi::{ComponentManifest, SlotKind};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct ProgramManifest {
    pub slots: Vec<ManifestSlot>,
    pub export: Option<String>,
    pub control: Option<String>,
    pub establishes_sessions: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ManifestSlot {
    pub kind: SlotKind,
    pub resource: u16,
    pub names: Vec<String>,
}

impl ProgramManifest {
    pub(crate) fn from_record(record: &ComponentManifest<'_>) -> Self {
        // Строки копируются: байты ELF живут только на время чтения файла, а
        // связывание идёт над всеми программами образа сразу.
        Self {
            slots: record
                .slots()
                .map(|slot| ManifestSlot {
                    kind: slot.kind(),
                    resource: slot.resource(),
                    names: slot.names().map(String::from).collect(),
                })
                .collect(),
            export: record.export().map(String::from),
            control: record.control().map(String::from),
            establishes_sessions: record.establishes_sessions(),
        }
    }
}

#[cfg(test)]
mod tests {
    use component_abi::{SlotDeclaration, decode_manifest, encode_manifest, manifest_len};

    use super::*;

    #[test]
    fn a_record_becomes_owned_strings() {
        const SLOTS: &[SlotDeclaration] = &[
            SlotDeclaration::userland(),
            SlotDeclaration::irq(&["arm,pl031", "arm,primecell"], 2),
            SlotDeclaration::service(&["Namespace"]),
            SlotDeclaration::control(&["NamespaceAdmin", "namespace-router"]),
        ];
        const LEN: usize = manifest_len(SLOTS, "FileSystem", "FileSystemAdmin");
        let bytes: [u8; LEN] = encode_manifest(SLOTS, "FileSystem", "FileSystemAdmin", true);

        let manifest = ProgramManifest::from_record(&decode_manifest(&bytes).expect("decodes"));

        assert_eq!(manifest.export.as_deref(), Some("FileSystem"));
        assert_eq!(manifest.control.as_deref(), Some("FileSystemAdmin"));
        assert!(manifest.establishes_sessions);
        assert_eq!(manifest.slots.len(), 4);
        assert_eq!(manifest.slots[0].kind, SlotKind::Userland);
        assert!(manifest.slots[0].names.is_empty());
        assert_eq!(manifest.slots[1].kind, SlotKind::Irq);
        assert_eq!(manifest.slots[1].resource, 2);
        assert_eq!(manifest.slots[1].names, ["arm,pl031", "arm,primecell"]);
        assert_eq!(manifest.slots[2].kind, SlotKind::Service);
        assert_eq!(manifest.slots[2].names, ["Namespace"]);
        assert_eq!(manifest.slots[3].kind, SlotKind::Control);
        assert_eq!(
            manifest.slots[3].names,
            ["NamespaceAdmin", "namespace-router"]
        );
    }
}
