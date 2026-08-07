//! Единый тип ошибки старта и подъёма файловых систем под оператор `?`.

use core::fmt::{Display, Formatter, Result as FmtResult};

use filesystem_abi::FileSystemError;
use ipc::wire::IpcError;
use userland_image::ImageDecodeError;

#[derive(Debug)]
pub(crate) enum Error {
    Runtime(runtime::Error),
    Ipc(IpcError),
    Image(ImageDecodeError),
    NoProgram,
    Start,
    MountTable,
}

pub(crate) type Result<T> = core::result::Result<T, Error>;

impl Error {
    pub(crate) fn as_file_system_error(&self) -> FileSystemError {
        match self {
            Self::NoProgram | Self::Image(_) | Self::MountTable => FileSystemError::NotFound,
            _ => FileSystemError::NoSpace,
        }
    }

    pub(crate) const fn exit_code(&self) -> u32 {
        match self {
            Self::Runtime(_) => 2,
            Self::Ipc(_) => 3,
            Self::Image(_) => 4,
            Self::NoProgram => 5,
            Self::Start => 6,
            Self::MountTable => 8,
        }
    }
}

impl Display for Error {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> FmtResult {
        match self {
            Self::Runtime(error) => write!(formatter, "runtime error {error:?}"),
            Self::Ipc(error) => write!(formatter, "ipc error {error:?}"),
            Self::Image(error) => write!(formatter, "image error {error:?}"),
            Self::NoProgram => formatter.write_str("no such program in the image"),
            Self::Start => formatter.write_str("no start frame"),
            Self::MountTable => formatter.write_str("boot mount table not applied"),
        }
    }
}

impl From<runtime::Error> for Error {
    fn from(error: runtime::Error) -> Self {
        Self::Runtime(error)
    }
}

impl From<IpcError> for Error {
    fn from(error: IpcError) -> Self {
        Self::Ipc(error)
    }
}

impl From<ImageDecodeError> for Error {
    fn from(error: ImageDecodeError) -> Self {
        Self::Image(error)
    }
}
