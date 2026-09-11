//! Объявляет макрос [`protocol!`] для объявления IPC-контракта, макрос [`wire_value!`] для
//! составного значения одного поля и wire-формат кадра, которым обмениваются стороны
//! контракта.
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
//! Сервер отвечает на `call` в одной из двух форм, и форму выбирает объявление операции.
//! Обработчик либо возвращает значение ответа, и ответ отправляет диспетчер, либо получает
//! долг ответа [`Reply`] и отвечает им сам - сразу либо позже. Ответ уходит каналом
//! [`ReplyChannel`], который транспорт доставляет вместе с кадром вызова.
//!
//! Тип записи поля читается [`WireValue`], пишется [`IntoWireValue`] и описывается в схеме
//! [`WireTyped`]. Значение, владеющее capability, пишется тем же [`IntoWireValue`], и итог
//! его отправки решает судьбу этих capability.
//!
//! Крейт обходится без зависимостей с crates.io и без proc-макросов. Он попадает в граф
//! зависимостей стандартной библиотеки, где не собирается ни то ни другое.

#![cfg_attr(not(test), no_std)]
#![cfg_attr(test, allow(clippy::cast_possible_truncation))]

#[cfg(test)]
extern crate std;

mod aggregate;
mod codec;
mod ordinal;
mod protocol;
mod reply;
mod resources;
pub mod schema;
pub mod transport;
pub mod wire;

pub use codec::{IntoWireValue, WireTyped, WireValue};
pub use ordinal::ordinal_of;
pub use protocol::Protocol;
#[doc(hidden)]
pub use protocol::{__protocol_answer, __protocol_call, __protocol_send};
pub use reply::Reply;
pub use resources::{Received, ReceivedResources};
#[doc(hidden)]
pub use syscall as __syscall;
pub use syscall::Timeout;
pub use transport::{
    Channel, FrameBytes, Incoming, Listener, RecvError, ReplyChannel, Undelivered,
};
