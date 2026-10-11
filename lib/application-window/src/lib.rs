//! Сторона приложения протокола `Application`.
//!
//! [`accept_window`] принимает заявку родителя на экспорт `Application` и возвращает
//! [`ApplicationWindow`] с окном и вводом из вызова `show` и сессией `Application`. Сессию
//! обслуживает [`ApplicationSession::serve`] в отдельном потоке. Он возвращается, когда открывший
//! приложение закрывает сессию, и после этого программа завершается.
//!
//! Программа на Rust со стандартной библиотекой получает окно через `slint-display`.

#![no_std]

mod window;

pub use window::{ApplicationSession, ApplicationWindow, WindowError, accept_window};
