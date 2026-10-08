//! Требования манифеста C-программы по стартовому handle процесса.

use component_startup::{Requirements, StartupError, StartupFailure, kind};

/// Строит требования `T`, объявленные [`component_manifest!`](crate::component_manifest),
/// по стартовому handle процесса.
///
/// Первый вызов забирает стартовый handle у рантайма и выдаёт требования, последующие вызовы
/// дают `NoStartHandle` с пустым `requirement`. Процесс без стартового handle получает тот же
/// отказ. Handle остаётся открытым до завершения процесса, и поля результата обращаются через
/// него к стартовой сессии.
pub fn requirements<T: Requirements>() -> Result<T, StartupError> {
    runtime::take_bootstrap()
        .map(|start| T::from_parent(start.assume_kind::<kind::Port>().leak()))
        .ok_or(StartupError {
            requirement: "",
            failure: StartupFailure::NoStartHandle,
        })
}
