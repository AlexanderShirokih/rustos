//! Требования компонента для программы на стандартной библиотеке.
//!
//! Программа объявляет манифест макросом [`component_manifest!`] и получает его требования
//! вызовом `components::requirements::<Manifest>()`. Журнал программе ставит точка
//! входа `std` до `main`, и крейт его не запрашивает.
//!
//! Владеющие типы программы и стандартной библиотеки объявлены в разных копиях графа.
//! [`into_std_handle`] переводит capability в тип `std`.

#![feature(molecular_ext)]

use std::os::molecular;

pub use component_startup::{Attributes, Providers, StartupError, component_manifest};
use component_startup::{Handle, Requirements, StartupFailure};

mod handle;

pub use handle::into_std_handle;

/// Строит требования манифеста `T` по стартовому handle. Без
/// стартового handle возвращает отказ [`StartupFailure::NoStartHandle`].
pub fn requirements<T: Requirements>() -> Result<T, StartupError> {
    // Стартовая сессия нужна до конца процесса, и её запись остаётся процессу.
    molecular::take_start_handle()
        .map(|start| T::from_parent(Handle::from_abi(start.leak())))
        .ok_or(StartupError {
            requirement: "",
            failure: StartupFailure::NoStartHandle,
        })
}
