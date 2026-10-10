//! Единый тип ошибки старта и цикла сервера под оператор `?`.

use core::fmt::{Display, Formatter, Result as FmtResult};

use filesystem::ImageFileSystemError;
use filesystem_abi::FileSystemError;
use ipc::wire::IpcError;
use syscall::SyscallError;

/// Отказ старта либо цикла обслуживания сервера.
#[derive(Debug)]
pub enum Error {
    /// Отказ рантайма: syscall, маппинг либо спавн потока.
    Runtime(kernel_objects::Error),
    /// Отказ транспорта при обмене кадрами.
    Ipc(IpcError),
    /// Доменный отказ файловой системы на пути подъёма.
    FileSystem(FileSystemError),
    /// Образ не разобран в дерево имён.
    Image(ImageFileSystemError),
    /// `start` не получил требование.
    Start,
}

pub type Result<T> = core::result::Result<T, Error>;

pub(crate) fn no_space(error: kernel_objects::Error) -> FileSystemError {
    let out_of_resource = matches!(
        error,
        kernel_objects::Error::Syscall(
            SyscallError::OutOfMemory
                | SyscallError::OutOfMappings
                | SyscallError::OutOfHandles
                | SyscallError::ResourceExhausted
        )
    );
    if !out_of_resource {
        runtime::log_fmt(format_args!("memory refused: {error:?}"));
    }
    FileSystemError::NoSpace
}

impl Display for Error {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> FmtResult {
        match self {
            Self::Runtime(error) => write!(formatter, "runtime error {error:?}"),
            Self::Ipc(error) => write!(formatter, "ipc error {error:?}"),
            Self::FileSystem(error) => write!(formatter, "file system error {error:?}"),
            Self::Image(error) => write!(formatter, "image error {error:?}"),
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

impl From<IpcError> for Error {
    fn from(error: IpcError) -> Self {
        Self::Ipc(error)
    }
}

impl From<FileSystemError> for Error {
    fn from(error: FileSystemError) -> Self {
        Self::FileSystem(error)
    }
}

impl From<ImageFileSystemError> for Error {
    fn from(error: ImageFileSystemError) -> Self {
        Self::Image(error)
    }
}
