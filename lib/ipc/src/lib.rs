//! Объявляет макрос [`protocol!`] для объявления IPC-контракта, макрос [`wire_value!`] для
//! составного значения одного поля и wire-формат кадра, которым обмениваются стороны
//! контракта.
//!
//! Видов операции три. `call` - запрос с ответом, `cast` - односторонняя операция от клиента
//! к серверу, `event` - односторонняя операция от сервера к клиенту. Кадр операции состоит из
//! заголовка [`wire::Header`] с ординалом операции, тела из записей полей и вектора
//! capability, идущего мимо тела.
//!
//! Раскрытие [`protocol!`] порождает модуль с клиентом, серверным трейтом и диспетчером
//! кадра. Вызывающий реализует [`Transport`] поверх syscall-ов своего канала и передаёт эту
//! реализацию в `Client::new`. Ответ `call`-операции приходит в [`Response`] вместе с
//! владением capability кадра.
//!
//! Крейт обходится без зависимостей с crates.io и без proc-макросов. Он попадает в граф
//! зависимостей стандартной библиотеки, где не собирается ни то ни другое.

#![cfg_attr(not(test), no_std)]
#![cfg_attr(test, allow(clippy::cast_possible_truncation))]

#[cfg(test)]
extern crate std;

mod aggregate;
pub mod codec;
pub mod handle;
mod ordinal;
mod protocol;
mod response;
pub mod schema;
pub mod transport;
pub mod wire;

pub use codec::{WireReply, WireTyped, WireValue};
pub use handle::{IntoWireHandle, WireReplyHandle};
pub use ordinal::ordinal_of;
#[doc(hidden)]
pub use protocol::{__protocol_reject, __protocol_release};
pub use response::{ReceivedResources, Response};
pub use syscall::Timeout;
pub use transport::{MessageLen, Transport};
