//! Экранный контроллер Doom: размещение кнопок, перевод касаний и объединение клавиш.
//!
//! `Layout` задаёт общий поворот рисования и ввода. `InputState` принимает записи `Input`
//! и выдаёт переходы клавиш `doomkeys.h` после целого кадра устройства. Модель применяется
//! исполняемым файлом Doom и проверяется синтетическими кадрами в testrunner.

#![no_std]

extern crate alloc;

mod controller;
mod key;
mod layout;

pub use controller::{Controls, InputState, KeyEvent};
pub use layout::{Button, Control, Layout, Mode, Point};
