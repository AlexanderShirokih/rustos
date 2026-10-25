//! Объявляет макрос [`protocol!`] для объявления IPC-контракта, макросы [`wire_value!`] для
//! составного значения одного поля и [`wire_enum!`] для перечисления в одном поле, а также
//! wire-формат кадра, которым обмениваются стороны контракта.
//!
//! Видов операции два. `call` - запрос с ответом, `cast` - односторонняя операция от клиента
//! к серверу. Кадр операции состоит из заголовка [`wire::Header`] с ординалом операции, тела из
//! записей полей и вектора capability, идущего мимо тела.
//!
//! Раскрытие [`protocol!`] порождает модуль с клиентом, серверным трейтом и диспетчером
//! кадра. Клиент работает поверх [`Channel`] - канала с операциями `send` и `call`, сервер
//! принимает кадры из [`Listener`]. Прочитанное значение владеет capability, которые называют
//! его поля, а capability, не названные ни одним полем, закрываются вместе с кадром.
//!
//! Сервер отвечает на `call` в одной из двух форм, выбранной объявлением операции.
//! Обработчик либо возвращает значение ответа, и ответ отправляет диспетчер, либо получает
//! долг ответа [`Reply`] для делегирования. Ответ уходит каналом
//! [`ReplyChannel`], который транспорт доставляет вместе с кадром вызова.
//!
//! Тип записи поля читается [`WireValue`], пишется [`IntoWireValue`] и описывается в схеме
//! [`WireTyped`]. Значение, владеющее capability, пишется тем же [`IntoWireValue`]: запись
//! переносит владельцев capability в кадр, и кадр уходит в канал вместе с ними.
//!
//! Крейт обходится без зависимостей с crates.io и без proc-макросов. Он попадает в граф
//! зависимостей стандартной библиотеки, где не собирается ни то ни другое.

#![cfg_attr(not(test), no_std)]
#![feature(impl_trait_in_assoc_type)]
#![cfg_attr(test, allow(clippy::cast_possible_truncation))]

extern crate alloc;
#[cfg(test)]
extern crate std;

mod aggregate;
mod call_error;
mod codec;
mod enumeration;
mod operation;
mod ordinal;
mod owned_incoming;
mod protocol;
mod reply;
mod resources;
pub mod schema;
mod service_owner;
pub mod transport;
pub mod wire;

pub use call_error::CallError;
pub use codec::{IntoWireValue, WireTyped, WireValue};
pub use operation::{Call, Send};
pub use ordinal::ordinal_of;
pub use owned_incoming::OwnedIncoming;
#[doc(hidden)]
pub use protocol::{__protocol_answer, __protocol_outcome};
pub use protocol::{DEFAULT_OPERATION_TIMEOUT, Protocol};
pub use reply::Reply;
pub use resources::{Received, ReceivedResources};
#[doc(hidden)]
pub use service_owner::__Owned;
pub use service_owner::ServiceOwner;
#[doc(hidden)]
pub use syscall as __syscall;
pub use syscall::Timeout;
pub use transport::{
    AsyncChannel, Channel, FrameBytes, Incoming, Listener, Outgoing, OutgoingParts, RecvError,
    ReplyChannel, Undelivered, Unsent,
};
