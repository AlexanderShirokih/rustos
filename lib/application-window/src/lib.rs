//! Сторона приложения протокола `Application`.
//!
//! [`accept_window`] принимает заявку родителя на экспорт `Application` и возвращает
//! [`ApplicationWindow`] с окном и вводом из вызова `show` и сессией `Application`. Сессию
//! обслуживает задача executor. [`ApplicationSession::serve`] ждёт закрытия сессии,
//! после которого программа завершает работу.
//!
//! Программа на Rust со стандартной библиотекой получает окно через `slint-display`.

#![no_std]

extern crate alloc;

mod window;

pub use window::{ApplicationSession, ApplicationWindow, WindowError, accept_window};
