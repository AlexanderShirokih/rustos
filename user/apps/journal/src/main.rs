//! Просмотр системного журнала на экране. Программа экспортирует `Application` с именем
//! `journal`, получает окно и ввод вызовом `show` и показывает последние записи лога ядра на
//! всей поверхности окна по сетке моноширинного шрифта. Строки прокручиваются перетаскиванием,
//! и пока показана последняя строка, журнал следит за концом. Отказ подписки программа
//! показывает строкой с причиной, отказ окна её завершает.

mod entry;
mod follow;
mod view;

#[allow(
    unreachable_pub,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::todo,
    clippy::unwrap_used
)]
mod ui {
    slint::include_modules!();
}

pub(crate) use entry::main;
