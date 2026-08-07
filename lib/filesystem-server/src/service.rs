//! Обслуживание сервисного порта: трансляция операций контракта `FileSystem` в модель
//! `lib/filesystem` поверх состояния, разделённого с admin-потоком.

use alloc::{boxed::Box, vec::Vec};

use filesystem::{ClientId, FileSystemBackend, NodeId, OpenNode, Session, SessionTable, Window};
use filesystem_abi::{
    AccessMode, DirCursor, FileSystemError, FileSystemService, Listed, NodeDescriptor, NodeInfo,
    NodeKind, OpenFlags, Opened, PATH_MAX, Path, Written,
};
use ipc::wire::{Cap, Str};
use runtime::{MemoryAccess, MemoryRegion, MutexGuard, OwnedHandle, Resource};
use syscall::Handle;
use userland::USERLAND_PAGE_SIZE;

use crate::{buffer::DataBuffer, error::no_space};

const DONE: u32 = 0;

const APPEND: u64 = u64::MAX;

#[derive(Default)]
pub(crate) struct Attachment {
    buffer: Option<DataBuffer>,
}

pub(crate) struct FileSystemState {
    backend: Box<dyn FileSystemBackend + Send>,
    sessions: SessionTable<Attachment>,
    resource: Resource,
    image: MemoryRegion,
}

#[derive(Default)]
pub(crate) struct Frame {
    badge: u64,
    received: Vec<OwnedHandle>,
    vended: Option<OwnedHandle>,
}

pub(crate) struct Serving<'a> {
    pub(crate) state: MutexGuard<'a, FileSystemState>,
    pub(crate) frame: &'a mut Frame,
}

impl FileSystemState {
    pub(crate) fn new(
        backend: Box<dyn FileSystemBackend + Send>,
        image: MemoryRegion,
        resource: Resource,
    ) -> Self {
        Self {
            backend,
            sessions: SessionTable::new(),
            resource,
            image,
        }
    }

    pub(crate) fn open_session(&mut self, badge: u64) -> Result<(), FileSystemError> {
        self.sessions.open(ClientId::from_raw(badge)).map(|_| ())
    }

    pub(crate) fn remove_session(&mut self, badge: u64) {
        // Нулевой значок ядро выдаёт кадру без значка: сессии за ним нет.
        if badge == 0 {
            return;
        }
        self.sessions.remove(ClientId::from_raw(badge));
    }
}

impl Frame {
    pub(crate) fn begin(&mut self, badge: u64, caps: impl IntoIterator<Item = Handle>) {
        self.badge = badge;
        self.received.clear();
        self.received
            .extend(caps.into_iter().map(OwnedHandle::from_handle));
    }

    pub(crate) fn end(&mut self, delivered: bool) {
        self.received.clear();
        settle(&mut self.vended, delivered);
    }
}

/// Разрешает судьбу выданной capability: доставленный ответ переносит её ядром получателю, а
/// недоставленный оставляет за сервером, и закрыть её больше некому.
pub fn settle(vended: &mut Option<OwnedHandle>, delivered: bool) {
    let Some(handle) = vended.take() else {
        return;
    };
    if delivered {
        let _ = handle.into_raw();
    } else {
        let _ = handle.close();
    }
}

/// Укладывает хэндл в кадр ответа, оставляя владение за отвечающим до конца кадра.
pub fn vend(vended: &mut Option<OwnedHandle>, handle: OwnedHandle) -> Cap {
    let id = handle.as_raw().as_nonzero();
    *vended = Some(handle);
    Cap::from_raw(id)
}

impl Serving<'_> {
    fn client(&self) -> Result<ClientId, FileSystemError> {
        if self.frame.badge == 0 {
            return Err(FileSystemError::NoSession);
        }
        Ok(ClientId::from_raw(self.frame.badge))
    }

    fn session(&mut self) -> Result<&mut Session<Attachment>, FileSystemError> {
        let client = self.client()?;
        self.state.sessions.get_mut(client)
    }

    fn backend_and_session(
        &mut self,
    ) -> Result<(&mut dyn FileSystemBackend, &mut Session<Attachment>), FileSystemError> {
        let client = self.client()?;
        let FileSystemState {
            backend, sessions, ..
        } = &mut *self.state;
        Ok((backend.as_mut(), sessions.get_mut(client)?))
    }

    fn buffer(session: &mut Session<Attachment>) -> Result<&mut DataBuffer, FileSystemError> {
        session
            .attachment_mut()
            .buffer
            .as_mut()
            .ok_or(FileSystemError::NoBuffer)
    }

    fn slice(&self, window: Window, mode: AccessMode) -> Result<OwnedHandle, FileSystemError> {
        let info = self.state.image.inspect().map_err(no_space)?;
        // Смещение окна кратно странице по построению образа, а его длина - нет: маппинг
        // округляется вверх и бывает длиннее файла.
        let size = window.size.div_ceil(USERLAND_PAGE_SIZE) * USERLAND_PAGE_SIZE;
        self.state
            .image
            .slice(window.offset, size, access_for(mode, info.access))
            .map(MemoryRegion::into_handle)
            .map_err(no_space)
    }
}

impl FileSystemService for Serving<'_> {
    fn acquire_buffer(&mut self, size: u32) -> Result<Cap, FileSystemError> {
        let client = self.client()?;
        let FileSystemState {
            resource, sessions, ..
        } = &mut *self.state;
        let session = sessions.get_mut(client)?;
        // Буфер заводится один раз за сессию: раскладка под уже выданной клиенту капой
        // переживает любой вызов.
        if session.attachment().buffer.is_some() {
            return Err(FileSystemError::AlreadyExists);
        }
        let buffer = DataBuffer::create(resource, size)?;
        let shared = buffer.share()?;
        session.attachment_mut().buffer = Some(buffer);
        Ok(vend(&mut self.frame.vended, shared))
    }

    fn open(
        &mut self,
        path: Str<'_, PATH_MAX>,
        mode: AccessMode,
        flags: OpenFlags,
    ) -> Result<Opened, FileSystemError> {
        let path = Path::new(path.as_str())?;
        let (backend, _) = self.backend_and_session()?;
        if mode.writable() && !backend.is_mutable() {
            return Err(FileSystemError::ReadOnly);
        }
        let opened = open_in_backend(backend, path, mode, flags)?;
        let node = opened.node;
        let info = backend.metadata(node)?;
        let fd = match self.session()?.open(OpenNode { node, mode }) {
            Ok(fd) => fd,
            Err(error) => {
                // Узел, заведённый ради этого дескриптора, не переживает отказ его выдачи:
                // откат не даёт отказному вызову оставить в дереве узел без дескриптора.
                if opened.created {
                    let (backend, _) = self.backend_and_session()?;
                    let _ = backend.unlink(path);
                }
                return Err(error);
            }
        };
        Ok(Opened {
            fd,
            kind: info.kind,
            size: info.size,
        })
    }

    fn close(&mut self, fd: NodeDescriptor) -> Result<u32, FileSystemError> {
        self.session()?.close(fd)?;
        Ok(DONE)
    }

    fn read(&mut self, fd: NodeDescriptor, offset: u64, len: u32) -> Result<u32, FileSystemError> {
        let (backend, session) = self.backend_and_session()?;
        let open_node = readable(session.get(fd)?)?;
        let count = Self::buffer(session)?.publish(len as usize, |out| {
            backend.read_at(open_node.node, offset, out)
        })?;
        Ok(count as u32)
    }

    fn write(
        &mut self,
        fd: NodeDescriptor,
        offset: u64,
        len: u32,
    ) -> Result<Written, FileSystemError> {
        let (backend, session) = self.backend_and_session()?;
        let open_node = writable(session.get(fd)?)?;
        Self::buffer(session)?.snapshot(len as usize, |data| {
            if offset == APPEND {
                let at = backend.append(open_node.node, data)?;
                // Сколько байт легло, знает бекенд: дозапись отдаёт только их смещение, а длину
                // даёт выросшая длина узла.
                let size = backend.metadata(open_node.node)?.size;
                return Ok(Written {
                    count: size.saturating_sub(at) as u32,
                    offset: at,
                });
            }
            let count = backend.write_at(open_node.node, offset, data)? as u32;
            Ok(Written { count, offset })
        })
    }

    fn set_size(&mut self, fd: NodeDescriptor, size: u64) -> Result<u32, FileSystemError> {
        let (backend, session) = self.backend_and_session()?;
        let open_node = writable(session.get(fd)?)?;
        backend.set_size(open_node.node, size)?;
        Ok(DONE)
    }

    fn stat(&mut self, fd: NodeDescriptor) -> Result<NodeInfo, FileSystemError> {
        let (backend, session) = self.backend_and_session()?;
        let open_node = readable(session.get(fd)?)?;
        backend.metadata(open_node.node)
    }

    fn list(&mut self, fd: NodeDescriptor, from: DirCursor) -> Result<Listed, FileSystemError> {
        let (backend, session) = self.backend_and_session()?;
        let open_node = readable(session.get(fd)?)?;
        let buffer = Self::buffer(session)?;
        // Пачка записей занимает буфер целиком: сколько их уместилось, решает перечисление.
        let room = buffer.size();
        buffer.publish(room, |out| backend.list(open_node.node, from, out))
    }

    fn rename(
        &mut self,
        from: Str<'_, PATH_MAX>,
        to: Str<'_, PATH_MAX>,
    ) -> Result<u32, FileSystemError> {
        let (from, to) = (Path::new(from.as_str())?, Path::new(to.as_str())?);
        let (backend, _) = self.backend_and_session()?;
        backend.rename(from, to)?;
        Ok(DONE)
    }

    fn unlink(&mut self, path: Str<'_, PATH_MAX>) -> Result<u32, FileSystemError> {
        let path = Path::new(path.as_str())?;
        let (backend, _) = self.backend_and_session()?;
        backend.unlink(path)?;
        Ok(DONE)
    }

    fn backing_memory(&mut self, fd: NodeDescriptor) -> Result<Cap, FileSystemError> {
        let (backend, session) = self.backend_and_session()?;
        let open_node = readable(session.get(fd)?)?;
        let window = backend.backing(open_node.node)?;
        let slice = self.slice(window, open_node.mode)?;
        Ok(vend(&mut self.frame.vended, slice))
    }
}

fn access_for(mode: AccessMode, region: MemoryAccess) -> MemoryAccess {
    let requested = if mode.writable() {
        MemoryAccess::RW
    } else {
        MemoryAccess::READ
    };
    // Доступ производной капы шире регионного ядро отвергает: пересечение считает сервер.
    MemoryAccess::from_bits_truncate(requested.raw() & region.raw())
}

fn readable(open_node: OpenNode) -> Result<OpenNode, FileSystemError> {
    if open_node.mode.readable() {
        return Ok(open_node);
    }
    Err(FileSystemError::WrongMode)
}

fn writable(open_node: OpenNode) -> Result<OpenNode, FileSystemError> {
    if open_node.mode.writable() {
        return Ok(open_node);
    }
    Err(FileSystemError::WrongMode)
}

struct OpenedNode {
    node: NodeId,
    created: bool,
}

fn open_in_backend(
    backend: &mut dyn FileSystemBackend,
    path: Path<'_>,
    mode: AccessMode,
    flags: OpenFlags,
) -> Result<OpenedNode, FileSystemError> {
    // Усечение меняет узел и подчиняется тому же режиму, что и запись.
    if flags.contains(OpenFlags::TRUNCATE) && !mode.writable() {
        return Err(FileSystemError::WrongMode);
    }
    let opened = match backend.lookup(path) {
        Ok(_) if flags.contains(OpenFlags::CREATE | OpenFlags::EXCLUSIVE) => {
            return Err(FileSystemError::AlreadyExists);
        }
        Ok(node) => OpenedNode {
            node,
            created: false,
        },
        Err(FileSystemError::NotFound) if flags.contains(OpenFlags::CREATE) => {
            let kind = if flags.contains(OpenFlags::DIRECTORY) {
                NodeKind::Directory
            } else {
                NodeKind::File
            };
            OpenedNode {
                node: backend.create(path, kind)?,
                created: true,
            }
        }
        Err(error) => return Err(error),
    };

    if flags.contains(OpenFlags::DIRECTORY) && !backend.metadata(opened.node)?.kind.is_directory() {
        return Err(FileSystemError::NotADirectory);
    }
    if flags.contains(OpenFlags::TRUNCATE) {
        backend.set_size(opened.node, 0)?;
    }
    Ok(opened)
}
