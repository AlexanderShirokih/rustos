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
    Runtime(runtime::Error),
    /// Отказ транспорта при обмене кадрами.
    Ipc(IpcError),
    /// Доменный отказ файловой системы на пути подъёма.
    FileSystem(FileSystemError),
    /// Образ не разобран в дерево имён.
    Image(ImageFileSystemError),
    /// Стартовый кадр не принёс capability.
    Start,
}

pub type Result<T> = core::result::Result<T, Error>;

/// Сводит отказ ядра в памяти к `NoSpace`. Причину вне нехватки ресурса
/// (например, нехватку прав) контракт выразить не даёт, и она уходит
/// строкой в лог.
pub(crate) fn no_space(error: runtime::Error) -> FileSystemError {
    let out_of_resource = matches!(
        error,
        runtime::Error::Syscall(
            SyscallError::OutOfMemory
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
            Self::Start => formatter.write_str("no start frame"),
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
