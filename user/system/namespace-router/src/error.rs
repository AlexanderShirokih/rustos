//! Единый тип ошибки старта и цикла маршрутизатора под оператор `?`.

use core::fmt::{Display, Formatter, Result as FmtResult};

use component_startup::{StartupError, StartupFailure};
use ipc::wire::IpcError;
use syscall::SyscallError;

#[derive(Debug)]
pub(crate) enum Error {
    Runtime(kernel_objects::Error),
    Ipc(IpcError),
    Start,
}

pub(crate) type Result<T> = core::result::Result<T, Error>;

impl Display for Error {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> FmtResult {
        match self {
            Self::Runtime(error) => write!(formatter, "runtime error {error:?}"),
            Self::Ipc(error) => write!(formatter, "ipc error {error:?}"),
            Self::Start => formatter.write_str("start refused"),
        }
    }
}

impl From<kernel_objects::Error> for Error {
    fn from(error: kernel_objects::Error) -> Self {
        Self::Runtime(error)
    }
}

impl From<SyscallError> for Error {
    fn from(error: SyscallError) -> Self {
        Self::Runtime(error.into())
    }
}

impl From<StartupError> for Error {
    fn from(error: StartupError) -> Self {
        match error.failure {
            StartupFailure::Ipc(error) => Self::Ipc(error),
            StartupFailure::Session(_) | StartupFailure::NoStartHandle => Self::Start,
        }
    }
}
