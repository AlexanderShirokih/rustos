//! Wire-формат IPC-сообщений поверх Channel; ordinal вычисляет `ipc-macros`,
//! wire переносит готовое значение.

#![cfg_attr(not(test), no_std)]

#[cfg(test)]
extern crate std;

pub mod codec;
pub mod schema;
pub mod transport;
pub mod wire;

pub use codec::WireValue;
pub use ipc_macros::protocol;
pub use transport::{MessageLen, Transport};
