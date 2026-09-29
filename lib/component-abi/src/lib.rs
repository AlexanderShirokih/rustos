//! Протокол [`component_startup`], которым компонент получает ресурсы от родителя, и манифест
//! компонента в секции [`COMPONENT_MANIFEST_SECTION`] его исполняемого файла.

#![cfg_attr(not(test), no_std)]
#![cfg_attr(
    test,
    allow(
        clippy::cast_possible_truncation,
        clippy::std_instead_of_core,
        clippy::unwrap_in_result
    )
)]

#[cfg(test)]
extern crate std;

mod declare;
mod manifest;
mod startup;

#[doc(hidden)]
pub use ipc::Protocol as __Protocol;
pub use manifest::{
    COMPONENT_MANIFEST_SECTION, ComponentManifest, ManifestDecodeError, ManifestNames,
    ManifestRequirement, ManifestRequirements, RequirementDeclaration, RequirementMode,
    decode_manifest, encode_manifest, encode_manifest_into, manifest_len,
};
pub use startup::*;
