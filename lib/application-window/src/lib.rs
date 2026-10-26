//! Сторона приложения протокола `Application`.
//!
//! [`receive_window`] получает окно и ввод приложения. Программа обслуживает сессию через
//! [`ApplicationSession::serve`] и завершает процесс после его возврата.
//!
//! Программа на Rust со стандартной библиотекой получает окно через `slint-display`.

#![no_std]

extern crate alloc;

mod window;

pub use window::{ApplicationSession, ApplicationWindow, WindowError, receive_window};
