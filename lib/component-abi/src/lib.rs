//! Контракт запуска компонента композиции: одноразовый стартовый кадр, которым
//! supervisor отдаёт компоненту его исходные capability и запись конфигурации.

#![cfg_attr(not(test), no_std)]

#[cfg(test)]
extern crate std;

mod config;
mod startup;

pub use config::{COMPONENT_CONFIG_MAX, ComponentConfig};
pub use startup::*;
