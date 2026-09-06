use component_startup::StartupError;
use ipc::wire::IpcError;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// Отказ userspace runtime.
    Runtime,
    /// Отказ приёма либо отправки кадра.
    Ipc,
    /// Стартовая транзакция не сложилась в контракт.
    Start,
}

pub type Result<T> = core::result::Result<T, Error>;

impl From<kernel_objects::Error> for Error {
    fn from(_error: kernel_objects::Error) -> Self {
        Self::Runtime
    }
}

impl From<IpcError> for Error {
    fn from(_error: IpcError) -> Self {
        Self::Ipc
    }
}

impl From<StartupError> for Error {
    fn from(error: StartupError) -> Self {
        match error {
            StartupError::Transport(_) => Self::Ipc,
            _ => Self::Start,
        }
    }
}
