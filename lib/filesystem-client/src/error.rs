//! Исход операции фасада.

use filesystem_abi::FileSystemError;
use ipc::wire::IpcError;

/// Причина отказа операции фасада.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Failure {
    /// Отказ файловой подсистемы.
    FileSystem(FileSystemError),
    /// Отказ доставки кадра.
    Transport(IpcError),
}

/// Исход операции фасада.
pub type Result<T> = core::result::Result<T, Failure>;

impl Failure {
    pub(crate) const fn peer_gone(self) -> bool {
        matches!(
            self,
            Self::Transport(IpcError::PeerClosed | IpcError::SenderGone)
        )
    }

    pub(crate) const fn name_missing(self) -> bool {
        matches!(self, Self::FileSystem(FileSystemError::NotFound))
    }
}

impl From<FileSystemError> for Failure {
    fn from(error: FileSystemError) -> Self {
        Self::FileSystem(error)
    }
}

impl From<IpcError> for Failure {
    fn from(error: IpcError) -> Self {
        Self::Transport(error)
    }
}
