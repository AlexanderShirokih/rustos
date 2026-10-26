//! Платформа Slint над сессией `Display`.
//!
//! [`ScreenPlatform::open`] ставит окно Slint на весь экран с масштабом по плотности экрана,
//! [`ScreenPlatform::run`] выполняет цикл кадра задачей executor по событиям, таймерам и
//! анимациям Slint. [`ScreenPlatform::release`] отдаёт экран, [`ScreenPlatform::attach`] подключает
//! новую сессию экрана.
//!
//! [`ScreenPlatform::deliver_input`] переводит записи сессии `Input` в события окна.
//!
//! [`receive_window`] получает окно и сессию `Input` приложения вызовом `show` его экспорта
//! `Application`.
//!
//! Slint собран с `unsafe-single-threaded` и не проверяет поток, его объекты использует
//! только поток `open`. Другие потоки запускаются после `open` и передают работу через
//! `slint::Weak::upgrade_in_event_loop` либо `slint::invoke_from_event_loop`.

mod application;
mod event_loop;
mod input;
mod pixel;
mod scale;
mod screen;

pub use application::{Shown, receive_window};
pub use application_window::WindowError;
pub use screen::{ScreenError, ScreenPlatform};
