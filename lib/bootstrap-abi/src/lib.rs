//! Контракты первого процесса с ядром, [`bootstrap`] и [`hardware`], и контракты [`image`] и
//! [`features`] компонента с загрузочной программой.
//!
//! [`bootstrap::Client`] создаётся из handle, который ядро передаёт первому процессу
//! аргументом точки входа. Он пишет строки в журнал ядра и выдаёт регион userland-образа и
//! сессию [`hardware`]. [`hardware::Client`] выдаёт полномочия на устройства машины,
//! [`image::Client`] - регион userland-образа компоненту, а [`features::Client`] публикует
//! признаки машины.

#![cfg_attr(not(test), no_std)]

#[cfg(test)]
extern crate std;

mod bootstrap_session;
mod device_window;
mod features_session;
mod hardware_session;
mod image_session;
mod log;

pub use bootstrap_session::{LOG_MESSAGE_MAX, LOG_TAG_MAX, bootstrap};
pub use device_window::{ContainingWindow, DeviceWindow};
pub use features_session::{FEATURE_MAX_BYTES, FeaturesError, features};
pub use hardware_session::{BootFdt, hardware};
pub use image_session::{ImageError, image};
