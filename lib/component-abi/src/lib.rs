//! Контракт запуска компонента композиции: одноразовый стартовый кадр, которым
//! supervisor отдаёт компоненту его исходные capability и запись конфигурации.

#![cfg_attr(not(test), no_std)]

#[cfg(test)]
extern crate std;

mod config;
mod declare;
mod manifest;
mod startup;

pub use config::{COMPONENT_CONFIG_MAX, ComponentConfig};
pub use manifest::{
    COMPONENT_MANIFEST_HEADER_SIZE, COMPONENT_MANIFEST_MAGIC, COMPONENT_MANIFEST_SECTION,
    COMPONENT_MANIFEST_VERSION, ComponentManifest, ManifestDecodeError, SlotKind, decode_manifest,
    encode_manifest,
};
pub use startup::*;
