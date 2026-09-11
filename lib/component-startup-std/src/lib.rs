//! Старт компонента по стартовому handle стандартной библиотеки и перевод capability во
//! владеющие типы `std`.
//!
//! Владеющие типы программы и стандартной библиотеки объявлены в разных копиях графа.
//! `into_std_handle` переводит capability в тип `std`.
//!
//! Крейт собран со стандартной библиотекой. `component-startup` стандартной библиотеки
//! не требует.

#![feature(molecular_ext)]

use std::os::molecular;

use component_startup::{
    Handle, IpcError, LOG_REQUIREMENT, Startup, StartupError, StartupFailure, kind,
};

mod handle;

pub use handle::into_std_handle;

/// Запрашивает требования функцией `start` манифеста по стартовому handle `std` и передаёт
/// журнал в `std`. Без стартового handle функция возвращает отказ требования журнала с
/// `IpcError::PeerClosed`.
pub fn start<T>(
    start: impl FnOnce(Handle<kind::Port>) -> Result<Startup<T>, StartupError>,
) -> Result<T, StartupError> {
    // Стартовая сессия нужна до конца процесса, и её handle не закрывается.
    let parent = molecular::take_start_handle()
        .map(molecular::OwnedHandle::into_raw)
        .ok_or(StartupError {
            requirement: LOG_REQUIREMENT,
            failure: StartupFailure::Ipc(IpcError::PeerClosed),
        })?;

    let started = start(Handle::from_abi(parent))?;
    if let Some(log) = started.log {
        molecular::install_log(into_std_handle(log.into_handle()));
    }

    Ok(started.requirements)
}
