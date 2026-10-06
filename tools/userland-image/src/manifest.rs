use component_abi::RequirementMode;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct ProgramManifest {
    pub requirements: Vec<ManifestRequirement>,
    pub exports: Vec<ProgramExport>,
    /// Признаки машины, без которых экспорты программы скрыты.
    pub features: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ProgramExport {
    /// Идентификатор протокола экспорта.
    pub protocol: String,
    /// Фильтры экспорта, каждый - пары ключа и значения.
    pub filters: Vec<Vec<(String, String)>>,
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
        use component_abi::{
            Attribute, ExportDeclaration, RequirementDeclaration, encode_manifest_into,
            manifest_len,
        };
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

        let exports: Vec<_> = self
            .exports
            .iter()
            .map(|export| {
                let filters: Vec<&'static [Attribute<'static>]> = export
                    .filters
                    .iter()
                    .map(|filter| {
                        let pairs: Vec<_> = filter
                            .iter()
                            .map(|(key, value)| Attribute {
                                key: String::leak(key.clone()).as_bytes(),
                                value: String::leak(value.clone()).as_bytes(),
                            })
                            .collect();
                        &*Vec::leak(pairs)
                    })
                    .collect();
                ExportDeclaration::filtered(
                    String::leak(export.protocol.clone()),
                    Vec::leak(filters),
                )
            })
            .collect();

        let features: Vec<&str> = self.features.iter().map(String::as_str).collect();
        let mut manifest = vec![0; manifest_len(&requirements, &exports, &features)];
        encode_manifest_into(&mut manifest, &requirements, &exports, &features);

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
