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

use component_startup::{Port, ReceivedStartup, StartupError};

mod handle;

pub use handle::into_std_handle;

/// Принимает стартовую транзакцию по одноразовому стартовому handle стандартной библиотеки,
/// отдаёт стандартной библиотеке лог-сессию компонента и возвращает программе остальные
/// ресурсы.
///
/// `receive` - приёмник из объявления `component_startup::component_manifest!`.
pub fn receive<T>(
    receive: impl FnOnce(Port) -> Result<ReceivedStartup<T>, StartupError>,
) -> Result<T, StartupError> {
    let startup = molecular::take_start_handle().ok_or(StartupError::MissingStartHandle)?;
    // Стартовый handle стандартной библиотеки содержит только номер записи. Вид порта задан
    // контрактом стартовой транзакции, и здесь он применяется к принятой записи.
    let startup = Port::from_handle(component_startup::Handle::from_abi(startup.into_raw()));

    let received = receive(startup)?;
    molecular::install_log(into_std_handle(received.log));
    Ok(received.resources)
}
