//! Сторона родителя протокола `ComponentStartup`.
//!
//! [`Children`] запускает дочерние компоненты и хранит отложенные ответы на их запросы.
//! Родитель сам решает, перезапускать ли компонент.

#![no_std]

extern crate alloc;

mod children;

pub use children::{Children, Departure, GrantError, Outcome, Status};
