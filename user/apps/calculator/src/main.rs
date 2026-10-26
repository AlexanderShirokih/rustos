//! Калькулятор на Slint. Программа экспортирует `Application` с именем `calculator` и получает
//! окно вызовом `show`. Над клавиатурой строка выражения и результат. Клавиши нажимаются
//! касанием либо печатаются с клавиатуры, Enter вычисляет, Escape сбрасывает, Backspace
//! стирает цифру.

mod calculator;
mod entry;

#[allow(
    unsafe_code,
    unreachable_pub,
    clippy::all,
    clippy::pedantic,
    clippy::nursery,
    clippy::restriction
)]
mod ui {
    slint::include_modules!();
}

pub(crate) use entry::main;
