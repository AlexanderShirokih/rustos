//! Ошибки сервиса создания user-процессов.

use drivers_common::services::scheduler::SpawnError;

use crate::image::UserImageError;

/// Ошибки `UserProcessLauncher::spawn_user_process_with_launch` и
/// `SchedulerService::spawn_user_process`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SpawnUserError {
    /// Scheduler создан без `address_space_factory` - user-AS создать нельзя.
    MissingFactory,
    /// Описание образа не прошло валидацию.
    Image(UserImageError),
    /// Bootstrap-аргумент ссылается на несуществующий initial handle.
    InvalidBootstrapHandle,
    /// Начальных handle'ов больше, чем может вместить таблица процесса.
    TooManyInitialHandles,
    /// Не удалось выделить ресурс через общий `SpawnError`.
    Spawn(SpawnError),
}

impl From<UserImageError> for SpawnUserError {
    fn from(value: UserImageError) -> Self {
        SpawnUserError::Image(value)
    }
}

impl From<SpawnError> for SpawnUserError {
    fn from(value: SpawnError) -> Self {
        SpawnUserError::Spawn(value)
    }
}
