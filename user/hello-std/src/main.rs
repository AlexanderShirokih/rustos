//! Программа на стандартной библиотеке Rust: печатает строку в системный журнал.
//!
//! Слотов ей не нужно, и стартовую транзакцию она принимает наравне с прочими компонентами.
//! Приём закрывает стартовое рандеву композиции и отдаёт библиотеке лог-сессию набора.

use std::process;

use component_startup::component_manifest;

const NO_STARTUP_EXIT_CODE: i32 = 1;

component_manifest! {
    struct HelloStartup {
        slots: {},
    }
}

fn main() {
    // До установки лог-сессии вывод не доходит до журнала, поэтому процесс называет отказ
    // приёма кодом выхода.
    let Ok(_startup) = component_startup_std::receive(HelloStartup::receive) else {
        process::exit(NO_STARTUP_EXIT_CODE);
    };

    println!("hello");
}
