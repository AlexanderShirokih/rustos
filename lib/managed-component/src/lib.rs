//! Сторона родителя протокола `ComponentStartup`.
//!
//! [`Children`] запускает дочерние компоненты и связывает заявки импортёров с ожидающими
//! провайдерами.
//! Родитель сам решает, перезапускать ли компонент.

#![no_std]

extern crate alloc;

mod children;
mod requests;

pub use children::{
    Children, Departure, GrantError, Held, IMPORTER_REQUESTS_PER_EXPORT_MAX, Outcome, Purpose,
    Status,
};
pub use requests::{Accept, ReturnRequest};
pub use userland_loader::ImagePages;
