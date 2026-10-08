//! Отдаёт программе образа её linker-script и компилирует её C-исходники на этапе сборки.
//!
//! [`emit_linker_script`] кладёт скрипт в `OUT_DIR` и печатает cargo-директиву линковщику.
//! [`compile_c`] собирает C-исходники пакета под библиотеку C, [`link_libc`] передаёт
//! компоновщику саму библиотеку. Пакет вызывает эти функции из своего `build.rs`.
//!
//! Скрипт задаёт раскладку сегментов программы и ставит секцию `.component.manifest` вне
//! загружаемой части.

mod c_sources;
mod error;
mod linker;

pub use c_sources::{compile_c, link_libc};
pub use error::Error;
pub use linker::emit_linker_script;
