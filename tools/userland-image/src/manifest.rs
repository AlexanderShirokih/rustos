use component_abi::RequirementMode;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct ProgramManifest {
    pub requirements: Vec<ManifestRequirement>,
    /// Протоколы экспортов в порядке объявления, по `Protocol::ID` на экспорт.
    pub exports: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ManifestRequirement {
    pub name: String,
    pub mode: RequirementMode,
    /// Идентификатор протокола требования.
    pub protocol: String,
}

impl ProgramManifest {
    pub(crate) fn program_bytes(&self) -> Vec<u8> {
        use component_abi::{RequirementDeclaration, encode_manifest_into, manifest_len};
        use userland::{Program, Segment, SegmentPermissions, encode_program};
        let requirements: Vec<_> = self
            .requirements
            .iter()
            .map(|requirement| {
                let name = String::leak(requirement.name.clone());
                let protocol = String::leak(requirement.protocol.clone());
                match requirement.mode {
                    RequirementMode::One => RequirementDeclaration::session(name, protocol),
                    RequirementMode::All => RequirementDeclaration::all(name, protocol),
                }
            })
            .collect();
        let exports: Vec<_> = self.exports.iter().map(String::as_str).collect();
        let mut manifest = vec![0; manifest_len(&requirements, &exports)];
        encode_manifest_into(&mut manifest, &requirements, &exports);
        encode_program(&Program {
            manifest: &manifest,
            entry_va: 4096,
            stack_size: 4096,
            segments: &[Segment {
                va_base: 4096,
                mem_size: 4096,
                permissions: SegmentPermissions::ReadExecute,
                bytes: b"CODE",
            }],
        })
        .expect("fixture program")
    }
}
