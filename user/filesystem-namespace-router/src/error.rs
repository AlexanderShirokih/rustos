//! Единый тип ошибки старта и подъёма файловых систем под оператор `?`.

use core::fmt::{Display, Formatter, Result as FmtResult};

use filesystem_abi::FileSystemError;
use ipc::wire::IpcError;
use userland_image::ImageDecodeError;

// Отказ старта маршрутизатора либо подъёма файловой системы.
#[derive(Debug)]
pub(crate) enum Error {
    // Отказ рантайма: syscall, маппинг либо спавн процесса.
    Runtime(runtime::Error),
    // Отказ транспорта при обмене кадрами.
    Ipc(IpcError),
    // Образ не разобран кодеком.
    Image(ImageDecodeError),
    // В образе нет программы с таким путём либо запись программой не является.
    NoProgram,
    // Стартовый кадр не принёс capability.
    Start,
}

pub(crate) type Result<T> = core::result::Result<T, Error>;

impl Error {
    /// Причина отказа в терминах контракта. Нехватку ресурса от прочих отказов ядра контракт
    /// различить не даёт, и подробность уходит строкой в лог.
    pub(crate) fn as_file_system_error(&self) -> FileSystemError {
        match self {
            Self::NoProgram | Self::Image(_) => FileSystemError::NotFound,
            _ => FileSystemError::NoSpace,
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
