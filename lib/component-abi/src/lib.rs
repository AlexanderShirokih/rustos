//! Контракты компонента композиции: стартовая транзакция, установление сессий и запись
//! манифеста в программе.
//!
//! Одноразовой транзакцией [`component_startup`] supervisor отдаёт компоненту его исходные
//! capability: лог-сессию, серверные стороны control- и service-портов, дальше по одной
//! capability на каждый слот. Принятое собрано в [`StartedComponent`].
//!
//! Контрактом [`component_sessions`] владелец control-порта запрашивает у компонента сессию его
//! service-порта, называя область, до которой сессию сузить. Сторону supervisor на
//! control-порту помечает значок [`CONTROL_LEASE_BADGE`]. Уход этой стороны распознаёт
//! [`supervisor_gone`].
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
