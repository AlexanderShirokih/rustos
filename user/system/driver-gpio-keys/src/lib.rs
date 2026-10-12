//! Кнопки на линиях GPIO по биндингу `gpio-keys`: клавиши потомков узла и окно программного
//! антидребезга линии. Правила биндинга повторяют `drivers/input/keyboard/gpio_keys.c` Linux
//! 6.12.
#![no_std]

extern crate alloc;

mod keys;
mod window;

pub use keys::{Key, Keys, KeysError, keys};
pub use window::{Edge, Window};
