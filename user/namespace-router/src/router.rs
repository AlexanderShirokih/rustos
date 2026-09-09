//! Таблица монтирования и её обслуживание с двух портов. Admin-сторона меняет соответствие
//! префиксов сессиям работающих файловых систем, а service-сторона разбирает пути и запрашивает
//! у выбранной файловой системы свежую клиентскую сессию.

use alloc::{string::String, vec::Vec};

use filesystem_abi::{
    FileSystemError, PATH_MAX, Path, ResolvedPair, common_directory, file_system, namespace,
    namespace_admin,
};
use ipc::wire::{Cap, Str};
use kernel_objects::{OwnedHandle, Port, ReceivedFrame, kind, settle, vend};
use runtime::{Mutex, PortTransport, Timeout};

const DONE: u32 = 0;
const OPEN_ROOT_TIMEOUT: Timeout = Timeout::from_secs(1);

struct Mount {
    prefix: String,
    session: Port,
}

pub(crate) struct MountTable {
    mounts: Vec<Mount>,
}

impl MountTable {
    pub(crate) const fn new() -> Self {
        Self { mounts: Vec::new() }
    }

    fn longest_prefix(&self, path: Path<'_>) -> Option<usize> {
        // Позиция различает записи, и два префикса одной файловой системы дают разные
        // монтирования.
        self.mounts
            .iter()
            .enumerate()
            .filter(|(_, mount)| {
                Path::new(&mount.prefix).is_ok_and(|prefix| path.strip_prefix(prefix).is_some())
            })
            .max_by_key(|(_, mount)| mount.prefix.len())
            .map(|(index, _)| index)
    }

    fn covers(&self, prefix: Path<'_>) -> bool {
        self.mounts
            .iter()
            .any(|mount| mount.prefix == prefix.as_str())
    }
}

pub(crate) struct Resolving<'a> {
    table: &'a Mutex<MountTable>,
    vended: Option<OwnedHandle<kind::Any>>,
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

    fn open_scope(
        &mut self,
        mount: &Mount,
        tail: Path<'_>,
    ) -> Result<Cap<kind::Port>, FileSystemError> {
        let mut response = file_system::Client::new(PortTransport::client(&mount.session))
            .with_wait(OPEN_ROOT_TIMEOUT)
            .open_root(Str::new(tail.as_str()).ok_or(FileSystemError::InvalidPath)?)
            .map_err(|_| FileSystemError::Unresponsive)?;
        let session = response.value?;
        let owned = response
            .claim(session)
            .ok_or(FileSystemError::Unresponsive)?;
        Ok(vend(&mut self.vended, Port::from(owned)))
    }
}

impl namespace::Service for Resolving<'_> {
    fn resolve(&mut self, path: Str<'_, PATH_MAX>) -> Result<Cap<kind::Port>, FileSystemError> {
        let path = Path::new(path.as_str())?;
        let table = self.table.lock();
        let index = table
            .longest_prefix(path)
            .ok_or(FileSystemError::NotFound)?;
        let mount = table.mounts.get(index).ok_or(FileSystemError::NotFound)?;
        let prefix = Path::new(&mount.prefix)?;
        let tail = path.strip_prefix(prefix).ok_or(FileSystemError::NotFound)?;
        self.open_scope(mount, tail)
    }

    fn resolve_pair(
        &mut self,
        from: Str<'_, PATH_MAX>,
        to: Str<'_, PATH_MAX>,
    ) -> Result<ResolvedPair, FileSystemError> {
        let from = Path::new(from.as_str())?;
        let to = Path::new(to.as_str())?;
        let table = self.table.lock();
        let first = table
            .longest_prefix(from)
            .ok_or(FileSystemError::NotFound)?;
        let second = table.longest_prefix(to).ok_or(FileSystemError::NotFound)?;
        if first != second {
            return Err(FileSystemError::CrossMount);
        }
        let mount = table.mounts.get(first).ok_or(FileSystemError::NotFound)?;
        let prefix = Path::new(&mount.prefix)?;
        // Общий каталог пары может лежать выше корня монтирования, и сессия не поднимается над ним.
        let common = common_directory(from, to)?;
        let scope = common.strip_prefix(prefix).map_or(prefix, |_| common);
        let tail = scope
            .strip_prefix(prefix)
            .ok_or(FileSystemError::NotFound)?;

        let session = self.open_scope(mount, tail)?;
        let root_len =
            u32::try_from(scope.as_str().len()).map_err(|_| FileSystemError::InvalidPath)?;
        Ok(ResolvedPair { session, root_len })
    }
}

pub(crate) struct Administering<'a, 'f> {
    table: &'a Mutex<MountTable>,
    received: &'a mut ReceivedFrame<'f>,
}

impl<'a, 'f> Administering<'a, 'f> {
    pub(crate) fn new(table: &'a Mutex<MountTable>, received: &'a mut ReceivedFrame<'f>) -> Self {
        Self { table, received }
    }
}

impl namespace_admin::Service for Administering<'_, '_> {
    fn mount(
        &mut self,
        path: Str<'_, PATH_MAX>,
        session: Cap<kind::Port>,
    ) -> Result<u32, FileSystemError> {
        let prefix = Path::new(path.as_str())?;
        // Отвергнутое монтирование уносит переданную сессию: вернуть её отправителю нечем.
        let session = self
            .received
            .claim(session)
            .ok_or(FileSystemError::NotSupported)?;
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
