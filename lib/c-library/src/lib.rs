//! Рантайм C-программы и системная граница библиотеки C поверх рантайма процесса `runtime`.
//!
//! Крейт даёт C-программе точку входа, кучу, завершение, стандартные потоки в журнале процесса
//! и файлы по абсолютным путям от файлового корня [`install_filesystem_root`], потоки POSIX, их
//! синхронизацию и замки библиотеки C. Функции библиотеки C должны вызывать главный поток
//! программы и потоки `pthread_create`.
//!
//! Программа подключает крейт макросом [`c_program!`] в корне своего no_std-пакета. Её
//! C-функция `main` получает `argc`, равный 1, и имя bin-таргета в `argv[0]`. Требования
//! манифеста [`component_manifest!`] крейт выдаёт через [`requirements`].

#![no_std]
#![feature(allocator_api)]

extern crate alloc;

mod cond;
mod descriptor;
mod errno;
mod files;
mod key;
mod lock;
mod memory;
mod mutex;
mod once;
mod process;
mod program;
mod root;
mod startup;
mod stdio;
mod thread;

pub use component_startup::component_manifest;
pub use program::ProgramName;
pub use root::install_filesystem_root;
pub use startup::requirements;
