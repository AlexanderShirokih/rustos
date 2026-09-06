//! Объявляет макрос [`component_manifest!`] для публикации манифеста приложения и набор
//! вспомогательных функций для взаимодействия с компонентами манифеста.
//!
//! Макрос порождает по объявлению структуру, поля которой - владеющие хэндлы: control-порт,
//! service-порт, по полю на каждый слот манифеста. Метод `receive` этой структуры принимает
//! стартовую транзакцию по handle стартового порта и заполняет поля capability транзакции.
//!
//! Программа получает handle стартового порта при запуске. Программа на Rust со стандартной
//! библиотекой получает этот handle от `component-startup-std`.

#![no_std]

mod declare;
mod receive;

// Раскрытие `component_manifest!` вызывает макрос записи манифеста по этому пути, и
// объявляющая программа не объявляет своей зависимости на `component-abi`.
#[doc(hidden)]
pub use component_abi;
pub use kernel_objects::OwnedHandle;
pub use receive::{ReceivedStartup, StartupError, StartupSet, receive_set};
