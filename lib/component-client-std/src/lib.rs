//! Источник стартового порта у программы со стандартной библиотекой.
//!
//! Крейт отделён от `component-client` и собран со стандартной библиотекой, недоступной
//! freestanding-программе. Состояния процесса он не держит - стартовый handle и лог-сессия
//! принадлежат платформенному слою внутри `std`.

#![feature(molecular_ext)]

use std::os::molecular;

use component_client::{OwnedHandle, ReceivedStartup, StartupError};

mod handle;

pub use handle::into_std_handle;

/// Принимает стартовую транзакцию по одноразовому стартовому handle стандартной библиотеки,
/// отдаёт ей лог-сессию набора и возвращает программе остальные ресурсы.
///
/// `receive` - приёмник из объявления `component_client::component_manifest!`.
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
