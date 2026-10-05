use component_startup::{StartupError, StartupFailure};
use ipc::wire::IpcError;
use platform_abi::{DescriptionError, PlatformError};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Error {
    Runtime,
    Ipc,
    Start,
    Platform(PlatformError),
    Description(DescriptionError),
    NoDevice,
}

pub(crate) type Result<T> = core::result::Result<T, Error>;

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

impl From<PlatformError> for Error {
    fn from(error: PlatformError) -> Self {
        Self::Platform(error)
    }
}

impl From<DescriptionError> for Error {
    fn from(error: DescriptionError) -> Self {
        Self::Description(error)
    }
}

impl From<StartupError> for Error {
    fn from(error: StartupError) -> Self {
        match error.failure {
            StartupFailure::Ipc(_) => Self::Ipc,
            StartupFailure::Session(_) | StartupFailure::NoStartHandle => Self::Start,
        }
    }
}
