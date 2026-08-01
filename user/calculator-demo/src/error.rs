//! Единый тип ошибки демо: схлопывает разнородные сбои под оператор `?`.

use ipc::wire::IpcError;
use userland_image::{ImageDecodeError, ProgramDecodeError};

#[derive(Debug)]
pub enum Error {
    Runtime(runtime::Error),
    Ipc(IpcError),
    Decode(ImageDecodeError),
    Program(ProgramDecodeError),
    /// bootstrap отказал в выдаче ресурса.
    Vend,
    /// В образе нет записи с искомым путём.
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

impl From<ProgramDecodeError> for Error {
    fn from(error: ProgramDecodeError) -> Self {
        Self::Program(error)
    }
}
