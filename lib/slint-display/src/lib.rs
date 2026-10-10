//! Платформа Slint над сессией `Display`.
//!
//! [`ScreenPlatform::open`] ставит окно Slint на весь экран, [`ScreenPlatform::run`] крутит
//! цикл кадра в том же потоке по задачам, таймерам и анимациям Slint, [`scale_factor`]
//! выбирает масштаб окна по плотности экрана.
//! Slint собран с `unsafe-single-threaded` и не проверяет поток, его объекты использует
//! только поток `open`. Другие потоки запускаются после `open` и передают работу через
//! `slint::Weak::upgrade_in_event_loop` либо `slint::invoke_from_event_loop`.

mod event_loop;
mod pixel;
mod scale;
mod screen;

pub use scale::scale_factor;
pub use screen::{ScreenError, ScreenPlatform};
