//! Единый тип ошибки setup-фазы драйвера под оператор `?`.

use ipc::wire::IpcError;
use userland_image::ImageDecodeError;

#[derive(Debug)]
pub enum Error {
    Runtime(runtime::Error),
    Ipc(IpcError),
    Decode(ImageDecodeError),
    /// bootstrap отказал в выдаче ресурса.
    Vend,
    /// Boot FDT не распарсился, либо не содержит искомого устройства.
    Dtb,
    /// Окно устройства с искомой базой не найдено.
    NoDevice,
    /// В образе нет программы с искомым именем.
    MissingEntry,
}

pub type Result<T> = core::result::Result<T, Error>;

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
        Self::Decode(error)
    }
}
