//! Единый тип ошибки старта и цикла маршрутизатора под оператор `?`.

use core::fmt::{Display, Formatter, Result as FmtResult};

use ipc::wire::IpcError;

#[derive(Debug)]
pub(crate) enum Error {
    Runtime(runtime::Error),
    Ipc(IpcError),
    Start,
    MountTable,
}

pub(crate) type Result<T> = core::result::Result<T, Error>;

impl Error {
    pub(crate) const fn exit_code(&self) -> u32 {
        match self {
            Self::Runtime(_) => 2,
            Self::Ipc(_) => 3,
            Self::Start => 6,
            Self::MountTable => 7,
        }
    }
}

impl Display for Error {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> FmtResult {
        match self {
            Self::Runtime(error) => write!(formatter, "runtime error {error:?}"),
            Self::Ipc(error) => write!(formatter, "ipc error {error:?}"),
            Self::Start => formatter.write_str("no start frame"),
            Self::MountTable => formatter.write_str("boot mount table unavailable"),
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
