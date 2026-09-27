//! Программа на стандартной библиотеке Rust: печатает строку в системный журнал.
//!
//! У неё нет требований, и старт получает у родителя только журнал.

use std::process;

use component_startup::component_manifest;

const NO_STARTUP_EXIT_CODE: i32 = 1;

component_manifest! {
    struct HelloStartup {}
}

fn main() {
    // Без журнала вывод `println!` теряется, и отказ старта передаётся кодом выхода.
    let Ok(_requirements) = component_startup_std::start(HelloStartup::start) else {
        process::exit(NO_STARTUP_EXIT_CODE);
    };

    println!("hello");
}
