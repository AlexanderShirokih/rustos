//! Контракт запуска компонента композиции: одноразовый стартовый кадр, которым
//! supervisor отдаёт компоненту его исходные capability и запись конфигурации.

#![cfg_attr(not(test), no_std)]

#[cfg(test)]
extern crate std;

mod declare;
mod manifest;
mod startup;

pub use manifest::{
    COMPONENT_MANIFEST_HEADER_SIZE, COMPONENT_MANIFEST_MAGIC, COMPONENT_MANIFEST_SECTION,
    COMPONENT_MANIFEST_VERSION, COMPONENT_SLOT_HEADER_SIZE, ComponentManifest, ManifestDecodeError,
    ManifestSlot, ManifestSlots, SlotDeclaration, SlotKind, SlotNames, decode_manifest,
    encode_manifest, manifest_len,
};
pub use startup::*;
