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

use component_startup::{Handle, Requirements, StartupFailure};
pub use component_startup::{StartupError, component_manifest};

mod handle;

pub use handle::into_std_handle;

/// Запрашивает требования манифеста `T` по стартовому handle. Без
/// стартового handle возвращает отказ [`StartupFailure::NoStartHandle`].
pub fn requirements<T: Requirements>() -> Result<T, StartupError> {
    // Стартовая сессия нужна до конца процесса, и её запись остаётся процессу.
    let parent = molecular::take_start_handle()
        .map(molecular::OwnedHandle::leak)
        .ok_or(StartupError {
            requirement: "",
            failure: StartupFailure::NoStartHandle,
        })?;

    T::request(Handle::from_abi(parent))
}
