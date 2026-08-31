//! Таблица монтирования и её обслуживание с двух портов. Admin-сторона меняет соответствие
//! префиксов сессиям работающих файловых систем, а service-сторона разбирает пути и запрашивает
//! у выбранной файловой системы свежую клиентскую сессию.

use alloc::{string::String, vec::Vec};

use filesystem_abi::{FileSystemError, PATH_MAX, Path, file_system, namespace, namespace_admin};
use filesystem_server::{settle, vend};
use ipc::wire::{Cap, Str};
use kernel_objects::OwnedHandle;
use runtime::{Mutex, PortTransport, Timeout};
use syscall::Handle;

const DONE: u32 = 0;
const OPEN_ROOT_TIMEOUT: Timeout = Timeout::from_secs(1);

struct Mount {
    prefix: String,
    session: OwnedHandle,
}

pub(crate) struct MountTable {
    mounts: Vec<Mount>,
}

impl MountTable {
    pub(crate) const fn new() -> Self {
        Self { mounts: Vec::new() }
    }

    fn longest_prefix(&self, path: Path<'_>) -> Option<&Mount> {
        self.mounts
            .iter()
            .filter(|mount| {
                Path::new(&mount.prefix).is_ok_and(|prefix| path.strip_prefix(prefix).is_some())
            })
            .max_by_key(|mount| mount.prefix.len())
    }

    fn covers(&self, prefix: Path<'_>) -> bool {
        self.mounts
            .iter()
            .any(|mount| mount.prefix == prefix.as_str())
    }
}

pub(crate) struct Resolving<'a> {
    table: &'a Mutex<MountTable>,
    vended: Option<OwnedHandle>,
}

impl<'a> Resolving<'a> {
    pub(crate) fn new(table: &'a Mutex<MountTable>) -> Self {
        Self {
            table,
            vended: None,
        }
    }

    pub(crate) fn end_frame(&mut self, delivered: bool) {
        settle(&mut self.vended, delivered);
    }
}

impl namespace::Service for Resolving<'_> {
    fn resolve(&mut self, path: Str<'_, PATH_MAX>) -> Result<Cap, FileSystemError> {
        let path = Path::new(path.as_str())?;
        let table = self.table.lock();
        let mount = table
            .longest_prefix(path)
            .ok_or(FileSystemError::NotFound)?;
        let prefix = Path::new(&mount.prefix)?;
        let tail = path.strip_prefix(prefix).ok_or(FileSystemError::NotFound)?;
        let session = file_system::Client::new(PortTransport::client(&mount.session))
            .with_wait(OPEN_ROOT_TIMEOUT)
            .open_root(Str::new(tail.as_str()).ok_or(FileSystemError::InvalidPath)?)
            .map_err(|_| FileSystemError::Unresponsive)??;
        Ok(vend(&mut self.vended, OwnedHandle::adopt(session)))
    }
}

pub(crate) struct Administering<'a> {
    table: &'a Mutex<MountTable>,
    received: Vec<OwnedHandle>,
}

impl<'a> Administering<'a> {
    pub(crate) const fn new(table: &'a Mutex<MountTable>) -> Self {
        Self {
            table,
            received: Vec::new(),
        }
    }

    pub(crate) fn begin_frame(&mut self, capabilities: impl IntoIterator<Item = Handle>) {
        self.received.clear();
        self.received
            .extend(capabilities.into_iter().map(OwnedHandle::from_handle));
    }

    pub(crate) fn end_frame(&mut self) {
        self.received.clear();
    }

    fn take(&mut self, session: Cap) -> Result<OwnedHandle, FileSystemError> {
        let raw = session.raw().get();
        let index = self
            .received
            .iter()
            .position(|handle| handle.as_raw().raw() == raw)
            .ok_or(FileSystemError::NotSupported)?;
        Ok(self.received.swap_remove(index))
    }
}

impl namespace_admin::Service for Administering<'_> {
    fn mount(&mut self, path: Str<'_, PATH_MAX>, session: Cap) -> Result<u32, FileSystemError> {
        let prefix = Path::new(path.as_str())?;
        // Отвергнутое монтирование уносит переданную сессию: вернуть её отправителю нечем.
        let session = self.take(session)?;
        let mut table = self.table.lock();
        if table.covers(prefix) {
            return Err(FileSystemError::AlreadyExists);
        }
        table.mounts.push(Mount {
            prefix: String::from(prefix.as_str()),
            session,
        });
        Ok(DONE)
    }

    fn unmount(&mut self, path: Str<'_, PATH_MAX>) -> Result<u32, FileSystemError> {
        let prefix = Path::new(path.as_str())?;
        let mut table = self.table.lock();
        let index = table
            .mounts
            .iter()
            .position(|mount| mount.prefix == prefix.as_str())
            .ok_or(FileSystemError::NotFound)?;
        table.mounts.remove(index);
        Ok(DONE)
    }
}
