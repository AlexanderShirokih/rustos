use component_abi::{
    ComponentManifest, ManifestExport, ManifestRequirement, RequirementDeclaration,
};
use syscall::ProcessName;
use userland::DecodedProgram;

/// Описание компонента для добавления родителем. Строки и программа доступны до конца записи.
#[derive(Clone, Copy)]
pub struct Component<'a> {
    pub name: ProcessName<'a>,
    pub program: DecodedProgram<'a>,
    pub memory_limit: u64,
    pub restarts: u32,
}

/// Требование runtime-импортёра.
pub struct ImporterRequirement {
    pub declaration: RequirementDeclaration,
}

pub(crate) struct Record<'a> {
    pub(crate) name: &'a str,
    pub(crate) manifest: ComponentManifest<'a>,
    pub(crate) restarts: u32,
}

impl<'a> Record<'a> {
    pub(crate) fn requirement(&self, name: &str) -> Option<ManifestRequirement<'a>> {
        self.manifest
            .requirements()
            .find(|requirement| requirement.name() == name)
    }

    pub(crate) fn export(&self, protocol: &str) -> Option<ManifestExport<'a>> {
        self.manifest
            .exports()
            .find(|export| export.protocol() == protocol)
    }
}
