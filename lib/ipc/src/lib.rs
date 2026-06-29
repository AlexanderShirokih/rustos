//! Wire-формат IPC-сообщений поверх port-транспорта; ordinal
//! вычисляет `ipc-macros`, wire переносит готовое значение.

#![cfg_attr(not(test), no_std)]

#[cfg(test)]
extern crate std;

pub mod codec;
pub mod handle;
pub mod schema;
pub mod transport;
pub mod wire;

pub use codec::{WireTyped, WireValue};
pub use handle::IntoWireHandle;
pub use ipc_macros::{WireValue, protocol};
pub use transport::{MessageLen, Transport};
