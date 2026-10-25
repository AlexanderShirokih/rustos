//! Файловый менеджер на Slint. Программа экспортирует `Application` с именем `files`, получает
//! окно вызовом `show` и показывает смонтированные файловые системы, записи корня выбранного
//! тома, записи выбранного каталога и начало выбранного файла. Списки и начало файла
//! прокручиваются перетаскиванием, выбор клавишами прокручивает список к выбранной записи.

#![feature(molecular_ext)]

mod desktop;
mod entry;
mod navigation;
mod scan;

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
