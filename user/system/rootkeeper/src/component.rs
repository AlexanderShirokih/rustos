use alloc::string::String;

use bootstrap_abi::features;
use component_abi::{
    ComponentManifest, ManifestExport, ManifestRequirement, RequirementDeclaration,
};
use ipc::Protocol as _;
use syscall::ProcessName;
use userland::DecodedProgram;

/// Описание компонента для добавления родителем. Строки и программа доступны до конца записи.
#[derive(Clone, Copy)]
pub struct Component<'a> {
    pub name: ProcessName<'a>,
    /// Каталог программы в образе.
    pub executable: &'a str,
    pub program: DecodedProgram<'a>,
    pub memory_limit: u64,
    pub restarts: u32,
}

/// Требование runtime-импортёра.
pub struct ImporterRequirement {
    pub declaration: RequirementDeclaration,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Origin {
    Plan,
    Index,
}

pub(crate) struct Record<'a> {
    pub(crate) name: String,
    pub(crate) executable: &'a str,
    pub(crate) origin: Origin,
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

    pub(crate) fn publisher(&self) -> bool {
        self.origin == Origin::Plan
            && self
                .manifest
                .requirements()
                .any(|requirement| requirement.protocol() == features::Protocol::ID)
    }
}
