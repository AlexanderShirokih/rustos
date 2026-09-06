//! Принимает стартовую транзакцию по стартовому handle стандартной библиотеки и переводит
//! capability во владеющие типы `std`.
//!
//! Стартовый handle одноразовый и принадлежит платформенному слою `std`.
//! `receive` берёт этот handle у платформенного слоя, вызывает приёмник объявления
//! `component_manifest!`, сам устанавливает лог-сессию компонента в `std` и возвращает
//! программе остальные ресурсы.
//!
//! Владеющие типы программы и стандартной библиотеки объявлены в разных копиях графа.
//! `into_std_handle` переводит capability в тип `std`.
//!
//! Крейт собран со стандартной библиотекой. `component-startup` стандартной библиотеки
//! не требует.

#![feature(molecular_ext)]

use std::os::molecular;

use component_startup::{OwnedHandle, ReceivedStartup, StartupError};

mod handle;

pub use handle::into_std_handle;

/// Принимает стартовую транзакцию по одноразовому стартовому handle стандартной библиотеки,
/// отдаёт стандартной библиотеке лог-сессию компонента и возвращает программе остальные
/// ресурсы.
///
/// `receive` - приёмник из объявления `component_startup::component_manifest!`.
pub fn receive<T>(
    receive: impl FnOnce(OwnedHandle) -> Result<ReceivedStartup<T>, StartupError>,
) -> Result<T, StartupError> {
    let startup = molecular::take_start_handle().ok_or(StartupError::MissingStartHandle)?;
    let startup =
        OwnedHandle::from_raw(startup.into_raw().get()).ok_or(StartupError::MissingStartHandle)?;

    let received = receive(startup)?;
    molecular::install_log(into_std_handle(received.log));
    Ok(received.resources)
}
