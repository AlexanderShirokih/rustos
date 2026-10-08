//! Требования манифеста C-программы по стартовому handle процесса.

use component_startup::{Requirements, StartupError, StartupFailure, kind};

/// Запрашивает требования `T`, объявленные [`component_manifest!`](crate::component_manifest),
/// по стартовому handle процесса.
///
/// Первый вызов забирает стартовый handle у рантайма и выдаёт требования, последующие вызовы
/// дают `NoStartHandle`. Handle остаётся открытым до завершения процесса, и поле `parent`
/// результата обращается через него к стартовой сессии.
///
/// Отказы:
/// - `NoStartHandle` с пустым `requirement`: стартовый handle уже забран, либо процесс
///   запущен без него;
/// - отказ запроса требования из [`Requirements::request`].
pub fn requirements<T: Requirements>() -> Result<T, StartupError> {
    let parent = runtime::take_bootstrap()
        .map(|start| start.assume_kind::<kind::Port>().leak())
        .ok_or(StartupError {
            requirement: "",
            failure: StartupFailure::NoStartHandle,
        })?;
    T::request(parent)
}
