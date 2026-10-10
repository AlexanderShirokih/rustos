//! Устройство virtio-input в модели ввода.
//!
//! [`Device::describe`] строит дескриптор по разделам конфигурации, которые читает [`Config`].
//! [`Device::translate`] копит события устройства до конца кадра и переводит кадр в команду.
#![no_std]

extern crate alloc;

mod device;

pub use device::{Config, Device};
