//! Упругие столкновения шаров на Slint. Программа экспортирует `Application` с именем
//! `physics` и получает окно вызовом `show`. Поле шаров занимает экран над подписью, которая раз в
//! секунду показывает число шаров, шаги симуляции в секунду, время шага, частоту кадров и
//! время кадра. Симуляция идёт в своём потоке с постоянным шагом, начальная расстановка
//! шаров одинакова при каждом запуске.

mod entry;
mod physics;

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
