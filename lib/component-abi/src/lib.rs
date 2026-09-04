//! Контракты компонента композиции: одноразовый стартовый кадр, которым supervisor отдаёт
//! компоненту его исходные capability и запись конфигурации, установление сессий его
//! service-порта на control-порту вместе с их областью и значок, которым supervisor заклеймён
//! на control-порту.

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
mod sessions;
mod startup;

pub use manifest::{
    COMPONENT_MANIFEST_HEADER_SIZE, COMPONENT_MANIFEST_MAGIC, COMPONENT_MANIFEST_SECTION,
    COMPONENT_MANIFEST_VERSION, COMPONENT_SLOT_HEADER_SIZE, ComponentManifest, ManifestDecodeError,
    ManifestSlot, ManifestSlots, SlotDeclaration, SlotKind, SlotNames, decode_manifest,
    encode_manifest, manifest_len,
};
pub use sessions::{SESSION_SCOPE_MAX, SessionError, component_sessions};
pub use startup::*;
