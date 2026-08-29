//! Обслуживание сервисного порта: трансляция операций контракта `FileSystem` в модель
//! `lib/filesystem` поверх состояния, разделённого с control-потоком.

use alloc::{boxed::Box, string::String, vec::Vec};
use core::{
    num::NonZeroU64,
    sync::atomic::{AtomicU64, Ordering::Relaxed},
};

use filesystem::{ByteStore, ClientId, FileSystemBackend, NodeId, OpenNode, Session, SessionTable};
use filesystem_abi::{
    AccessMode, DirCursor, FileSystemError, Listed, NodeDescriptor, NodeInfo, NodeKind, OpenFlags,
    Opened, PATH_MAX, Path, Written, file_system, rebase,
};
use ipc::wire::{Cap, Str};
use runtime::{MemoryAccess, MemoryRegion, MutexGuard, OwnedHandle, Resource};
use syscall::{Handle, Rights};

use crate::{buffer::DataBuffer, error::no_space, store::StoreTable};

const DONE: u32 = 0;

const APPEND: u64 = u64::MAX;

#[derive(Default)]
pub(crate) struct Attachment {
    buffer: Option<DataBuffer>,
    scope: String,
}

pub(crate) struct FileSystemState {
    backend: Box<dyn FileSystemBackend + Send>,
    sessions: SessionTable<Attachment>,
    stores: StoreTable,
}

#[derive(Default)]
pub(crate) struct Frame {
    badge: u64,
    received: Vec<OwnedHandle>,
    vended: Option<OwnedHandle>,
    pending_pin: Option<NodeId>,
}

pub(crate) struct Serving<'a> {
    pub(crate) state: MutexGuard<'a, FileSystemState>,
    pub(crate) frame: &'a mut Frame,
    pub(crate) service: &'a OwnedHandle,
    pub(crate) next_badge: &'a AtomicU64,
}

pub(crate) fn mint_session(
    service: &OwnedHandle,
    next_badge: &AtomicU64,
) -> Result<(NonZeroU64, OwnedHandle), FileSystemError> {
    // Значения значков не переиспользуются: счётчик только растёт.
    let badge = NonZeroU64::MIN.saturating_add(next_badge.fetch_add(1, Relaxed));
    let session = service
        .duplicate(Rights::WRITE | Rights::TRANSFER, Some(badge))
        .map_err(no_space)?;
    Ok((badge, session))
}

fn scoped<'a>(
    scope: &str,
    tail: Str<'a, PATH_MAX>,
    out: &'a mut [u8; PATH_MAX],
) -> Result<Path<'a>, FileSystemError> {
    let tail = Path::new(tail.as_str())?;
    if scope.is_empty() {
        return Ok(tail);
    }
    rebase(Path::new(scope)?, tail, out)
}

fn scoped_entry<'a>(
    scope: &str,
    tail: Str<'a, PATH_MAX>,
    out: &'a mut [u8; PATH_MAX],
) -> Result<Path<'a>, FileSystemError> {
    // Корень окна - запись каталога над ним: операция над именем его не называет. Корень
    // монтирования у сессии без окна отвергает бекенд.
    if !scope.is_empty() && Path::new(tail.as_str())?.is_root() {
        return Err(FileSystemError::InvalidPath);
    }
    scoped(scope, tail, out)
}

impl FileSystemState {
    pub(crate) fn new(
        backend: Box<dyn FileSystemBackend + Send>,
        backing: Option<MemoryRegion>,
        resource: Resource,
    ) -> Self {
        Self {
            backend,
            sessions: SessionTable::new(),
            stores: StoreTable::new(resource, backing),
        }
    }

    pub(crate) fn open_session(&mut self, badge: u64) -> Result<(), FileSystemError> {
        self.sessions.open(ClientId::from_raw(badge)).map(|_| ())
    }

    pub(crate) fn open_session_at(
        &mut self,
        badge: u64,
        scope: &str,
    ) -> Result<(), FileSystemError> {
        let root = Path::new(scope)?;
        self.backend.lookup(root)?;
        self.open_session_scoped(badge, window_scope(root))
    }

    fn open_session_scoped(&mut self, badge: u64, scope: String) -> Result<(), FileSystemError> {
        let session = self.sessions.open(ClientId::from_raw(badge))?;
        session.attachment_mut().scope = scope;
        Ok(())
    }

    pub(crate) fn remove_session(&mut self, badge: u64) {
        // Нулевой значок ядро выдаёт кадру без значка: сессии за ним нет.
        if badge == 0 {
            return;
        }
        let Some(session) = self.sessions.remove(ClientId::from_raw(badge)) else {
            return;
        };
        for open_node in session.into_open_nodes() {
            self.backend.release(&mut self.stores, open_node.node);
        }
    }

    fn open_node(
        &mut self,
        client: ClientId,
        node: NodeId,
        mode: AccessMode,
    ) -> Result<NodeDescriptor, FileSystemError> {
        self.backend.retain(node)?;
        match self
            .sessions
            .get_mut(client)
            .and_then(|session| session.open(OpenNode { node, mode }))
        {
            Ok(fd) => Ok(fd),
            Err(error) => {
                self.backend.release(&mut self.stores, node);
                Err(error)
            }
        }
    }

    fn close_node(&mut self, client: ClientId, fd: NodeDescriptor) -> Result<(), FileSystemError> {
        let open_node = self.sessions.get_mut(client)?.close(fd)?;
        self.backend.release(&mut self.stores, open_node.node);
        Ok(())
    }
}

impl Frame {
    pub(crate) fn begin(&mut self, badge: u64, caps: impl IntoIterator<Item = Handle>) {
        self.badge = badge;
        self.received.clear();
        self.received
            .extend(caps.into_iter().map(OwnedHandle::from_handle));
    }

    pub(crate) fn end(&mut self, delivered: bool, state: &mut FileSystemState) {
        self.received.clear();
        if delivered {
            if let Some(node) = self.pending_pin.take() {
                let pinned = state.backend.pin(node);
                debug_assert!(pinned.is_ok());
            }
        } else {
            self.pending_pin = None;
        }
        settle(&mut self.vended, delivered);
    }

    fn take(&mut self, capability: Cap) -> Result<OwnedHandle, FileSystemError> {
        let raw = capability.raw().get();
        let index = self
            .received
            .iter()
            .position(|handle| handle.as_raw().raw() == raw)
            .ok_or(FileSystemError::NotSupported)?;
        Ok(self.received.swap_remove(index))
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

    fn backend_stores_session(
        &mut self,
    ) -> Result<
        (
            &mut dyn FileSystemBackend,
            &mut StoreTable,
            &mut Session<Attachment>,
        ),
        FileSystemError,
    > {
        let client = self.client()?;
        let FileSystemState {
            backend,
            stores,
            sessions,
        } = &mut *self.state;
        Ok((backend.as_mut(), stores, sessions.get_mut(client)?))
    }

    fn buffer(session: &mut Session<Attachment>) -> Result<&mut DataBuffer, FileSystemError> {
        session
            .attachment_mut()
            .buffer
            .as_mut()
            .ok_or(FileSystemError::NoBuffer)
    }
}

impl file_system::Service for Serving<'_> {
    fn acquire_buffer(&mut self, size: u32) -> Result<Cap, FileSystemError> {
        let client = self.client()?;
        let FileSystemState {
            stores, sessions, ..
        } = &mut *self.state;
        let session = sessions.get_mut(client)?;
        // Буфер заводится один раз за сессию: раскладка под уже выданной клиенту капой
        // переживает любой вызов.
        if session.attachment().buffer.is_some() {
            return Err(FileSystemError::AlreadyExists);
        }
        let buffer = DataBuffer::create(stores.resource(), size)?;
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
        let mut full = [0; PATH_MAX];
        let client = self.client()?;
        let (backend, stores, session) = self.backend_stores_session()?;
        let scope = &session.attachment().scope;
        // Корень окна - запись каталога над ним: сессия его открывает, но не заводит.
        let names_window_root = !scope.is_empty() && Path::new(path.as_str())?.is_root();
        let path = scoped(scope, path, &mut full)?;
        if mode.writable() && !backend.is_mutable() {
            return Err(FileSystemError::ReadOnly);
        }
        if names_window_root {
            backend.lookup(path)?;
        }
        let opened = open_in_backend(backend, stores, path, mode, flags)?;
        let node = opened.node;
        let info = backend.metadata(node)?;
        let fd = match self.state.open_node(client, node, mode) {
            Ok(fd) => fd,
            Err(error) => {
                // Узел, заведённый ради этого дескриптора, не переживает отказ его выдачи:
                // откат не даёт отказному вызову оставить в дереве узел без дескриптора.
                if opened.created {
                    let FileSystemState {
                        backend, stores, ..
                    } = &mut *self.state;
                    let _ = backend.unlink(stores, path);
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
        let client = self.client()?;
        self.state.close_node(client, fd)?;
        Ok(DONE)
    }

    fn read(&mut self, fd: NodeDescriptor, offset: u64, len: u32) -> Result<u32, FileSystemError> {
        let (backend, stores, session) = self.backend_stores_session()?;
        let open_node = readable(session.get(fd)?)?;
        let count = Self::buffer(session)?.publish(len as usize, |out| {
            backend.read_at(stores, open_node.node, offset, out)
        })?;
        Ok(count as u32)
    }

    fn write(
        &mut self,
        fd: NodeDescriptor,
        offset: u64,
        len: u32,
    ) -> Result<Written, FileSystemError> {
        let (backend, stores, session) = self.backend_stores_session()?;
        let open_node = writable(session.get(fd)?)?;
        Self::buffer(session)?.snapshot(len as usize, |data| {
            if offset == APPEND {
                let at = backend.append(stores, open_node.node, data)?;
                // Сколько байт легло, знает бекенд: дозапись отдаёт только их смещение, а длину
                // даёт выросшая длина узла.
                let size = backend.metadata(open_node.node)?.size;
                return Ok(Written {
                    count: size.saturating_sub(at) as u32,
                    offset: at,
                });
            }
            let count = backend.write_at(stores, open_node.node, offset, data)? as u32;
            Ok(Written { count, offset })
        })
    }

    fn set_size(&mut self, fd: NodeDescriptor, size: u64) -> Result<u32, FileSystemError> {
        let (backend, stores, session) = self.backend_stores_session()?;
        let open_node = writable(session.get(fd)?)?;
        backend.set_size(stores, open_node.node, size)?;
        Ok(DONE)
    }

    fn stat(&mut self, fd: NodeDescriptor) -> Result<NodeInfo, FileSystemError> {
        let (backend, _, session) = self.backend_stores_session()?;
        let open_node = readable(session.get(fd)?)?;
        backend.metadata(open_node.node)
    }

    fn list(&mut self, fd: NodeDescriptor, from: DirCursor) -> Result<Listed, FileSystemError> {
        let (backend, _, session) = self.backend_stores_session()?;
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
        let (mut full_from, mut full_to) = ([0; PATH_MAX], [0; PATH_MAX]);
        let (backend, stores, session) = self.backend_stores_session()?;
        let scope = &session.attachment().scope;
        let from = scoped_entry(scope, from, &mut full_from)?;
        let to = scoped_entry(scope, to, &mut full_to)?;
        backend.rename(stores, from, to)?;
        Ok(DONE)
    }

    fn unlink(&mut self, path: Str<'_, PATH_MAX>) -> Result<u32, FileSystemError> {
        let mut full = [0; PATH_MAX];
        let (backend, stores, session) = self.backend_stores_session()?;
        let path = scoped_entry(&session.attachment().scope, path, &mut full)?;
        backend.unlink(stores, path)?;
        Ok(DONE)
    }

    fn create_backed(
        &mut self,
        path: Str<'_, PATH_MAX>,
        region: Cap,
        size: u64,
    ) -> Result<u32, FileSystemError> {
        let client = self.client()?;
        let scope = self
            .state
            .sessions
            .get_mut(client)?
            .attachment()
            .scope
            .clone();
        let mut full = [0; PATH_MAX];
        let path = scoped_entry(&scope, path, &mut full)?;
        if !self.state.backend.is_mutable() {
            return Err(FileSystemError::ReadOnly);
        }

        let region = MemoryRegion::from_handle(self.frame.take(region)?);
        let FileSystemState {
            backend, stores, ..
        } = &mut *self.state;
        let (store, capacity) = stores.import(region)?;
        if let Err(error) = backend.create_backed(path, store, size, capacity) {
            stores.release(store);
            return Err(error);
        }
        Ok(DONE)
    }

    fn backing_memory(&mut self, fd: NodeDescriptor) -> Result<Cap, FileSystemError> {
        let (backend, stores, session) = self.backend_stores_session()?;
        let open_node = readable(session.get(fd)?)?;
        let window = backend.backing(stores, open_node.node)?;
        let slice = stores.slice(window, open_node.mode)?;
        self.frame.pending_pin = Some(open_node.node);
        Ok(vend(&mut self.frame.vended, slice))
    }

    fn open_root(&mut self, path: Str<'_, PATH_MAX>) -> Result<Cap, FileSystemError> {
        let mut full = [0; PATH_MAX];
        let (backend, _, session) = self.backend_stores_session()?;
        let root = scoped(&session.attachment().scope, path, &mut full)?;
        backend.lookup(root)?;
        let scope = window_scope(root);
        let (badge, capability) = mint_session(self.service, self.next_badge)?;
        // Сессия заводится до того, как её капа ушла клиенту: кадр по ней застанет
        // её заведённой.
        self.state.open_session_scoped(badge.get(), scope)?;
        Ok(vend(&mut self.frame.vended, capability))
    }
}

fn window_scope(root: Path<'_>) -> String {
    // Окно сессии - полный путь узла: сужение складывается, а расширить окно обратно
    // производной нечем.
    if root.is_root() {
        return String::new();
    }
    String::from(root.as_str())
}

pub(crate) fn access_for(mode: AccessMode, region: MemoryAccess) -> MemoryAccess {
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
    stores: &mut StoreTable,
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
        backend.set_size(stores, opened.node, 0)?;
    }
    Ok(opened)
}
