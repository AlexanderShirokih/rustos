//! Обслуживание сервисного порта: трансляция операций контракта `FileSystem` в модель
//! `lib/filesystem` поверх состояния, разделённого с потоками экспорта и монтирования.

use alloc::{boxed::Box, string::String};
use core::sync::atomic::AtomicU64;

use component_startup::{MintedSession, mint_session};
use filesystem::{
    ByteStore, ClientId, FileSystemBackend, NodeId, NodeMetadata, OpenNode, Session, SessionTable,
};
use filesystem_abi::{
    AccessMode, DirCursor, FileSystemError, Listed, NodeDescriptor, NodeInfo, NodeKind, OpenFlags,
    Opened, PATH_MAX, Path, Written, file_system, rebase,
};
use ipc::{IntoWireValue, wire::Cap};
use kernel_objects::{
    Delivery, IssuedHandle, MemoryRegion, OwnedHandle, Port, ReadOnlyMapping, Resource, kind,
};
use runtime::{MemoryAccess, random};

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
    volume: u64,
    sessions: SessionTable<Attachment>,
    stores: StoreTable,
}

#[derive(Default)]
pub(crate) struct Frame {
    pending_pin: Option<NodeId>,
}

pub(crate) struct Serving<'a> {
    pub(crate) state: &'a mut FileSystemState,
    pub(crate) frame: &'a mut Frame,
    pub(crate) service: &'a Port,
    pub(crate) next_badge: &'a AtomicU64,
}

fn scoped<'a>(
    scope: &str,
    tail: &'a str,
    out: &'a mut [u8; PATH_MAX],
) -> Result<Path<'a>, FileSystemError> {
    let tail = Path::new(tail)?;
    if scope.is_empty() {
        return Ok(tail);
    }
    rebase(Path::new(scope)?, tail, out)
}

fn scoped_entry<'a>(
    scope: &str,
    tail: &'a str,
    out: &'a mut [u8; PATH_MAX],
) -> Result<Path<'a>, FileSystemError> {
    // Корень окна - запись каталога над ним. Операция над именем его не называет. Корень
    // монтирования у сессии без окна отвергает бекенд.
    if !scope.is_empty() && Path::new(tail)?.is_root() {
        return Err(FileSystemError::InvalidPath);
    }
    scoped(scope, tail, out)
}

impl FileSystemState {
    pub(crate) fn new(
        backend: Box<dyn FileSystemBackend + Send>,
        backing: Option<&'static ReadOnlyMapping>,
        resource: Resource,
    ) -> Self {
        Self {
            backend,
            volume: volume_number(),
            sessions: SessionTable::new(),
            stores: StoreTable::new(resource, backing),
        }
    }

    pub(crate) fn serves(&self, badge: u64) -> bool {
        self.sessions.get(ClientId::from_raw(badge)).is_some()
    }

    pub(crate) fn open_session(&mut self, badge: u64) -> Result<(), FileSystemError> {
        self.sessions.open(ClientId::from_raw(badge)).map(|_| ())
    }

    fn open_session_scoped(&mut self, badge: u64, scope: String) -> Result<(), FileSystemError> {
        let session = self.sessions.open(ClientId::from_raw(badge))?;
        session.attachment_mut().scope = scope;
        Ok(())
    }

    pub(crate) fn remove_session(&mut self, badge: u64) {
        // Нулевой значок ядро выдаёт кадру без значка. За ним сессия отсутствует.
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
    pub(crate) fn end(&mut self, delivered: bool, state: &mut FileSystemState) {
        if delivered {
            if let Some(node) = self.pending_pin.take() {
                let pinned = state.backend.pin(node);
                debug_assert!(pinned.is_ok());
            }
        } else {
            self.pending_pin = None;
        }
    }
}

fn client_of(delivery: &Delivery) -> Result<ClientId, FileSystemError> {
    if delivery.badge == 0 {
        return Err(FileSystemError::NoSession);
    }
    Ok(ClientId::from_raw(delivery.badge))
}

impl Serving<'_> {
    fn backend_stores_session(
        &mut self,
        client: ClientId,
    ) -> Result<
        (
            &mut dyn FileSystemBackend,
            &mut StoreTable,
            &mut Session<Attachment>,
        ),
        FileSystemError,
    > {
        let FileSystemState {
            backend,
            stores,
            sessions,
            ..
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

    fn share_buffer(
        &mut self,
        client: ClientId,
        size: u32,
    ) -> Result<MemoryRegion, FileSystemError> {
        let FileSystemState {
            stores, sessions, ..
        } = &mut *self.state;
        let session = sessions.get_mut(client)?;
        // Буфер создаётся один раз за сессию. Раскладка под уже выданной клиенту капой
        // сохраняется при любом вызове.
        if session.attachment().buffer.is_some() {
            return Err(FileSystemError::AlreadyExists);
        }
        let buffer = DataBuffer::create(stores.resource(), size)?;
        let shared = buffer.share()?;
        session.attachment_mut().buffer = Some(buffer);
        Ok(shared)
    }

    fn backing_slice(
        &mut self,
        client: ClientId,
        fd: NodeDescriptor,
    ) -> Result<MemoryRegion, FileSystemError> {
        let (backend, stores, session) = self.backend_stores_session(client)?;
        let open_node = readable(session.get(fd)?)?;
        let window = backend.backing(stores, open_node.node)?;
        let slice = stores.slice(window, open_node.mode)?;
        self.frame.pending_pin = Some(open_node.node);
        Ok(slice)
    }

    fn mint_root(&mut self, client: ClientId, path: &str) -> Result<Port, FileSystemError> {
        let mut full = [0; PATH_MAX];
        let (backend, _, session) = self.backend_stores_session(client)?;
        let root = scoped(&session.attachment().scope, path, &mut full)?;
        backend.lookup(root)?;
        let scope = window_scope(root);
        let MintedSession { badge, port } =
            mint_session(self.service, self.next_badge).map_err(no_space)?;
        // Сессия создаётся до того, как её капа ушла клиенту. Кадр по ней застанет
        // её созданной.
        self.state.open_session_scoped(badge.get(), scope)?;
        Ok(port)
    }
}

impl file_system::Service<Delivery> for Serving<'_> {
    fn acquire_buffer(
        &mut self,
        delivery: &Delivery,
        size: u32,
    ) -> impl IntoWireValue<Delivery, Read = Result<Cap<kind::MemoryRegion>, FileSystemError>> {
        client_of(delivery).and_then(|client| self.share_buffer(client, size))
    }

    fn open(
        &mut self,
        delivery: &Delivery,
        path: &str,
        mode: AccessMode,
        flags: OpenFlags,
    ) -> Result<Opened, FileSystemError> {
        let mut full = [0; PATH_MAX];
        let client = client_of(delivery)?;
        let (backend, stores, session) = self.backend_stores_session(client)?;
        let scope = &session.attachment().scope;
        // Корень окна - запись каталога над ним. Сессия его открывает, но не создаёт.
        let names_window_root = !scope.is_empty() && Path::new(path)?.is_root();
        let path = scoped(scope, path, &mut full)?;
        if mode.writable() && !backend.is_mutable() {
            return Err(FileSystemError::ReadOnly);
        }
        if names_window_root {
            backend.lookup(path)?;
        }
        let opened = open_in_backend(backend, stores, path, mode, flags)?;
        let node = opened.node;
        let metadata = backend.metadata(node)?;
        let fd = match self.state.open_node(client, node, mode) {
            Ok(fd) => fd,
            Err(error) => {
                // Узел, заведённый ради этого дескриптора, не сохраняется при отказе его выдачи.
                // Откат не даёт отказному вызову оставить в дереве узел без дескриптора.
                if opened.created {
                    let FileSystemState {
                        backend, stores, ..
                    } = &mut *self.state;
                    let _ = backend.unlink(stores, path, metadata.kind);
                }
                return Err(error);
            }
        };
        Ok(Opened {
            fd,
            info: node_info(self.state.volume, node, metadata),
        })
    }

    fn close(&mut self, delivery: &Delivery, fd: NodeDescriptor) -> Result<u32, FileSystemError> {
        let client = client_of(delivery)?;
        self.state.close_node(client, fd)?;
        Ok(DONE)
    }

    fn read(
        &mut self,
        delivery: &Delivery,
        fd: NodeDescriptor,
        offset: u64,
        len: u32,
    ) -> Result<u32, FileSystemError> {
        let (backend, stores, session) = self.backend_stores_session(client_of(delivery)?)?;
        let open_node = readable(session.get(fd)?)?;
        let count = Self::buffer(session)?.publish(len as usize, |out| {
            backend.read_at(stores, open_node.node, offset, out)
        })?;
        u32::try_from(count).map_err(|_| FileSystemError::BufferTooSmall)
    }

    fn write(
        &mut self,
        delivery: &Delivery,
        fd: NodeDescriptor,
        offset: u64,
        len: u32,
    ) -> Result<Written, FileSystemError> {
        let (backend, stores, session) = self.backend_stores_session(client_of(delivery)?)?;
        let open_node = writable(session.get(fd)?)?;
        Self::buffer(session)?.snapshot(len as usize, |data| {
            if offset == APPEND {
                let at = backend.append(stores, open_node.node, data)?;
                // Бекенд определяет число уложенных байт. Дозапись отдаёт только их смещение, а
                // длину даёт выросшая длина узла.
                let size = backend.metadata(open_node.node)?.size;
                let count = u32::try_from(size.saturating_sub(at))
                    .map_err(|_| FileSystemError::BufferTooSmall)?;
                return Ok(Written { count, offset: at });
            }
            let written = backend.write_at(stores, open_node.node, offset, data)?;
            let count = u32::try_from(written).map_err(|_| FileSystemError::BufferTooSmall)?;
            Ok(Written { count, offset })
        })
    }

    fn set_size(
        &mut self,
        delivery: &Delivery,
        fd: NodeDescriptor,
        size: u64,
    ) -> Result<u32, FileSystemError> {
        let (backend, stores, session) = self.backend_stores_session(client_of(delivery)?)?;
        let open_node = writable(session.get(fd)?)?;
        backend.set_size(stores, open_node.node, size)?;
        Ok(DONE)
    }

    fn stat(
        &mut self,
        delivery: &Delivery,
        fd: NodeDescriptor,
    ) -> Result<NodeInfo, FileSystemError> {
        let volume = self.state.volume;
        let (backend, _, session) = self.backend_stores_session(client_of(delivery)?)?;
        let open_node = readable(session.get(fd)?)?;
        let metadata = backend.metadata(open_node.node)?;

        Ok(node_info(volume, open_node.node, metadata))
    }

    fn list(
        &mut self,
        delivery: &Delivery,
        fd: NodeDescriptor,
        from: DirCursor,
    ) -> Result<Listed, FileSystemError> {
        let (backend, _, session) = self.backend_stores_session(client_of(delivery)?)?;
        let open_node = readable(session.get(fd)?)?;
        let buffer = Self::buffer(session)?;
        // Пачка записей занимает буфер целиком. Сколько их уместилось, решает перечисление.
        let room = buffer.size();
        buffer.publish(room, |out| backend.list(open_node.node, from, out))
    }

    fn rename(
        &mut self,
        delivery: &Delivery,
        from: &str,
        to: &str,
    ) -> Result<u32, FileSystemError> {
        let (mut full_from, mut full_to) = ([0; PATH_MAX], [0; PATH_MAX]);
        let (backend, stores, session) = self.backend_stores_session(client_of(delivery)?)?;
        let scope = &session.attachment().scope;
        let from = scoped_entry(scope, from, &mut full_from)?;
        let to = scoped_entry(scope, to, &mut full_to)?;
        backend.rename(stores, from, to)?;
        Ok(DONE)
    }

    fn unlink(
        &mut self,
        delivery: &Delivery,
        path: &str,
        expected: NodeKind,
    ) -> Result<u32, FileSystemError> {
        let mut full = [0; PATH_MAX];
        let (backend, stores, session) = self.backend_stores_session(client_of(delivery)?)?;
        let path = scoped_entry(&session.attachment().scope, path, &mut full)?;
        backend.unlink(stores, path, expected)?;
        Ok(DONE)
    }

    fn create_backed(
        &mut self,
        delivery: &Delivery,
        path: &str,
        region: IssuedHandle<kind::MemoryRegion>,
        size: u64,
    ) -> Result<u32, FileSystemError> {
        // Регион закрывается на любом отказе операции.
        let region = OwnedHandle::from(region);
        let client = client_of(delivery)?;
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

    fn backing_memory(
        &mut self,
        delivery: &Delivery,
        fd: NodeDescriptor,
    ) -> impl IntoWireValue<Delivery, Read = Result<Cap<kind::MemoryRegion>, FileSystemError>> {
        client_of(delivery).and_then(|client| self.backing_slice(client, fd))
    }

    fn open_root(
        &mut self,
        delivery: &Delivery,
        path: &str,
    ) -> impl IntoWireValue<
        Delivery,
        Read = Result<ipc::wire::Session<file_system::Protocol>, FileSystemError>,
    > {
        // Порт сессии создаёт этот сервер, и он же обслуживает на нём `file_system`.
        client_of(delivery)
            .and_then(|client| self.mint_root(client, path))
            .map(kernel_objects::Session::from_handle)
    }
}

fn window_scope(root: Path<'_>) -> String {
    // Окно сессии - полный путь узла. Сужение складывается, а расширить окно обратно
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
    // Доступ производной капы шире регионного ядро отвергает. Пересечение считает сервер.
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

fn volume_number() -> u64 {
    let mut bytes = [0; size_of::<u64>()];
    random::fill_bytes(&mut bytes);

    u64::from_ne_bytes(bytes).max(1)
}

fn node_info(volume: u64, node: NodeId, metadata: NodeMetadata) -> NodeInfo {
    NodeInfo {
        kind: metadata.kind,
        size: metadata.size,
        volume,
        node: u64::from(node.raw()),
    }
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
