//! Контракты IPC: wire-формат сообщений и макрос, разворачивающий объявление контракта в
//! клиента, сервис и диспетчер.
//!
//! Крейт не тянет ни одной зависимости с crates.io и обходится без proc-макросов: он попадает
//! в граф зависимостей стандартной библиотеки, а там ни то ни другое собраться не может.

#![cfg_attr(not(test), no_std)]
#![cfg_attr(test, allow(clippy::cast_possible_truncation))]

#[cfg(test)]
extern crate std;

mod aggregate;
pub mod codec;
pub mod handle;
mod ordinal;
mod protocol;
pub mod schema;
pub mod transport;
pub mod wire;

pub use codec::{WireReply, WireTyped, WireValue};
pub use handle::{IntoWireHandle, WireReplyHandle};
pub use ordinal::ordinal_of;
#[doc(hidden)]
pub use protocol::{__protocol_reject, __protocol_release};
pub use syscall::Timeout;
pub use transport::{MessageLen, Transport};
