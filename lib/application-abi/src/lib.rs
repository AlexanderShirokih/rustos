//! Предоставляет контракт `Application`, по которому приложения получают окна от диспетчера окон.
//!
//! Клиента и сервис даёт модуль [`application`]. Окно - сессия `Display`, которую обслуживает
//! диспетчер окон. Вместе с окном приложение получает сессию `Input` его ввода.

#![cfg_attr(not(test), no_std)]

mod error;
mod protocol;

pub use error::ApplicationError;
pub use protocol::application;
