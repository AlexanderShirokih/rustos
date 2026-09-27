//! Требования компонента для программы на стандартной библиотеке.
//!
//! Программа объявляет манифест макросом [`component_manifest!`] и получает его требования
//! вызовом `components::requirements(Manifest::request)`. Журнал программе ставит точка
//! входа `std` до `main`, и крейт его не запрашивает.
//!
//! Владеющие типы программы и стандартной библиотеки объявлены в разных копиях графа.
//! [`into_std_handle`] переводит capability в тип `std`.

#![feature(molecular_ext)]

use std::os::molecular;

use component_startup::{Handle, StartupFailure, kind};
pub use component_startup::{StartupError, component_manifest};

mod handle;

pub use handle::into_std_handle;

/// Передаёт стартовый handle функции `request` манифеста и возвращает её требования. Без
/// стартового handle возвращает отказ [`StartupFailure::NoStartHandle`].
pub fn requirements<T>(
    request: impl FnOnce(Handle<kind::Port>) -> Result<T, StartupError>,
) -> Result<T, StartupError> {
    // Стартовая сессия нужна до конца процесса, и её handle не закрывается.
    let parent = molecular::take_start_handle()
        .map(molecular::OwnedHandle::into_raw)
        .ok_or(StartupError {
            requirement: "",
            failure: StartupFailure::NoStartHandle,
        })?;

    request(Handle::from_abi(parent))
}
