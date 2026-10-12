//! Контракт `Platform`, по которому драйверы могут читать описание платформы и получать линии
//! прерываний и окна регистров устройств.
//!
//! Клиента и сервис даёт модуль [`platform`]. [`DeviceId`] называет устройство путём его узла в
//! описании. [`Description`] читает описание из выданного региона и перечисляет устройства.
//! [`MappedDescription`] отображает выданное описание и держит его регион.
//! Выданные описание, линия и регион отзываются с концом сессии. [`CONTROLLER_KEY`] выбирает
//! поставщика ресурсов узла по совместимости его контроллера, [`DeviceClaims`] ведёт узлы,
//! занятые сессиями такого поставщика.

#![cfg_attr(not(test), no_std)]

extern crate alloc;

mod attribute;
mod claims;
mod description;
mod device;
mod error;
mod mapped;
mod protocol;
mod window;

pub use attribute::CONTROLLER_KEY;
pub use claims::DeviceClaims;
pub use description::{Ancestor, Description, DescriptionRegion, Device, Devices, Node, Specifier};
pub use device::{DEVICE_ID_MAX, DeviceId};
pub use error::{DescriptionError, PlatformError};
pub use mapped::{MappedDescription, WindowRefusal};
pub use protocol::platform;
pub use window::MmioWindow;
