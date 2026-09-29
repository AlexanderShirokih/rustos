use component_abi::{ComponentManifest, RequirementKind};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct ProgramManifest {
    pub requirements: Vec<ManifestRequirement>,
    /// Протоколы экспортов в порядке объявления, по `Protocol::ID` на экспорт.
    pub exports: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ManifestRequirement {
    pub name: String,
    pub kind: RequirementKind,
    /// Идентификатор протокола требования сессии.
    pub protocol: Option<String>,
}

impl ProgramManifest {
    pub(crate) fn from_record(record: &ComponentManifest<'_>) -> Self {
        // Строки копируются, потому что байты ELF доступны только на время чтения файла, а
        // связывание идёт над всеми программами образа сразу.
        Self {
            requirements: record
                .requirements()
                .map(|requirement| ManifestRequirement {
                    name: String::from(requirement.name()),
                    kind: requirement.kind(),
                    protocol: requirement.protocol().map(String::from),
                })
                .collect(),
            exports: record.exports().map(String::from).collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use component_abi::{RequirementDeclaration, decode_manifest, encode_manifest, manifest_len};

    use super::*;

    #[test]
    fn a_record_becomes_owned_strings() {
        const REQUIREMENTS: &[RequirementDeclaration] = &[
            RequirementDeclaration::region("image"),
            RequirementDeclaration::session("names", "Namespace"),
        ];
        const EXPORTS: &[&str] = &["FileSystem", "FileSystemAdmin"];
        const LEN: usize = manifest_len(REQUIREMENTS, EXPORTS);
        let bytes: [u8; LEN] = encode_manifest(REQUIREMENTS, EXPORTS);

        let manifest = ProgramManifest::from_record(&decode_manifest(&bytes).expect("decodes"));

        assert_eq!(manifest.exports, ["FileSystem", "FileSystemAdmin"]);
        assert_eq!(manifest.requirements.len(), 2);
        let names: Vec<&str> = manifest
            .requirements
            .iter()
            .map(|requirement| requirement.name.as_str())
            .collect();
        assert_eq!(names, ["image", "names"]);
        assert_eq!(manifest.requirements[0].kind, RequirementKind::Region);
        assert_eq!(manifest.requirements[0].protocol, None);
        assert_eq!(manifest.requirements[1].kind, RequirementKind::Session);
        assert_eq!(
            manifest.requirements[1].protocol.as_deref(),
            Some("Namespace")
        );
    }
}
