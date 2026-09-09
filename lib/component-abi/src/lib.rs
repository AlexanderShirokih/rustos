//! Контракты компонента композиции: стартовая транзакция, установление сессий и запись
//! манифеста в программе.
//!
//! Единственным портом супервизора компонент принимает [`component_startup`]: транзакцией
//! приходят лог-сессия, серверные стороны портов его экспортов и по capability на каждый
//! слот, а после транзакции по тому же порту супервизор запрашивает сессию названного
//! экспорта. Принятое транзакцией собрано в [`StartedComponent`].
//!
//! Запись манифеста компонента лежит в секции [`COMPONENT_MANIFEST_SECTION`] собранной
//! программы. Раскладку записи читает [`decode_manifest`]. Вид слота назван [`SlotKind`].

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

pub use manifest::{
    COMPONENT_MANIFEST_HEADER_SIZE, COMPONENT_MANIFEST_MAGIC, COMPONENT_MANIFEST_SECTION,
    COMPONENT_MANIFEST_VERSION, COMPONENT_SLOT_HEADER_SIZE, ComponentManifest, ManifestDecodeError,
    ManifestNames, ManifestSlot, ManifestSlots, SlotDeclaration, SlotKind, decode_manifest,
    encode_manifest, manifest_len,
};
pub use startup::*;
