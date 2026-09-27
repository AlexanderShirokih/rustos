//! Поведение фасада на парном транспорте: разбор пути и кэш сессий, буфер данных, дескрипторы
//! с их позицией, обход каталога и операции над деревом.

#![allow(
    clippy::cast_possible_truncation,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::std_instead_of_core,
    clippy::unwrap_in_result
)]

use std::{
    collections::BTreeMap,
    string::String,
    sync::{
        Arc, Mutex, MutexGuard,
        atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering},
    },
    thread,
    vec::Vec,
};

use filesystem_abi::{
    APPEND_OFFSET, AccessMode, DirCursor, DirEntry, FileSystemError, Listed, NAME_MAX,
    NodeDescriptor, NodeInfo, NodeKind, OpenFlags, Opened, PATH_MAX, Path, ResolvedPair, Written,
    common_directory, file_system, namespace,
};
use filesystem_client::{Environment, Failure, FileSystem, SharedRegion};
use ipc::{
    Channel, FrameBytes, Incoming, Listener, Timeout,
    wire::{Cap, HEADER_SIZE, IpcError, MESSAGE_INLINE_MAX, Str},
};
use ipc_test::{MockChannel, MockListener, MockResources};
use syscall::{Handle, ObjectKind, kind};

const BUFFER_SIZE: u32 = 4096;

const ROOT: &str = "/";

const TRUNCATED_BODY: usize = 4;

type Shared = Arc<Mutex<Vec<u8>>>;

#[derive(Clone, PartialEq, Eq)]
enum Node {
    File(Vec<u8>),
    Directory,
}

struct Filesystem {
    tree: Mutex<BTreeMap<String, Node>>,
}

struct Mount {
    prefix: String,
    filesystem: Arc<Filesystem>,
}

struct World {
    mounts: Vec<Mount>,
    sessions: Mutex<BTreeMap<u32, MockChannel>>,
    buffers: Mutex<BTreeMap<u32, Shared>>,
    resolved: Mutex<Vec<String>>,
    pair_root_len: Mutex<Option<u32>>,
    protected: Mutex<Option<String>>,
    swapped: Mutex<Option<(String, Node)>>,
    next_cap: AtomicU32,
    reads: AtomicUsize,
    live: AtomicUsize,
    transports: AtomicUsize,
    poison: AtomicBool,
    scoping_fails: AtomicBool,
    claims_fail: AtomicBool,
    truncates_pair: AtomicBool,
    mappings_allowed: AtomicUsize,
    transfers_allowed: AtomicUsize,
}

impl Filesystem {
    fn new(entries: &[(&str, Node)]) -> Arc<Self> {
        let mut tree = BTreeMap::new();
        tree.insert(String::from(ROOT), Node::Directory);
        for (path, node) in entries {
            tree.insert(String::from(*path), node.clone());
        }
        Arc::new(Self {
            tree: Mutex::new(tree),
        })
    }

    fn tree(&self) -> MutexGuard<'_, BTreeMap<String, Node>> {
        self.tree.lock().expect("mutex not poisoned")
    }
}

impl World {
    fn new(entries: &[(&str, Node)]) -> Arc<Self> {
        Self::mounted(&[(ROOT, entries)])
    }

    fn mounted(mounts: &[(&str, &[(&str, Node)])]) -> Arc<Self> {
        Arc::new(Self {
            mounts: mounts
                .iter()
                .map(|(prefix, entries)| Mount {
                    prefix: String::from(*prefix),
                    filesystem: Filesystem::new(entries),
                })
                .collect(),
            sessions: Mutex::new(BTreeMap::new()),
            buffers: Mutex::new(BTreeMap::new()),
            resolved: Mutex::new(Vec::new()),
            pair_root_len: Mutex::new(None),
            protected: Mutex::new(None),
            swapped: Mutex::new(None),
            next_cap: AtomicU32::new(1),
            reads: AtomicUsize::new(0),
            live: AtomicUsize::new(0),
            transports: AtomicUsize::new(0),
            poison: AtomicBool::new(false),
            scoping_fails: AtomicBool::new(false),
            claims_fail: AtomicBool::new(false),
            truncates_pair: AtomicBool::new(false),
            mappings_allowed: AtomicUsize::new(usize::MAX),
            transfers_allowed: AtomicUsize::new(usize::MAX),
        })
    }

    fn tree(&self) -> MutexGuard<'_, BTreeMap<String, Node>> {
        self.root_filesystem().tree()
    }

    fn root_filesystem(&self) -> &Arc<Filesystem> {
        &self
            .mounts
            .first()
            .expect("the world carries at least one mount")
            .filesystem
    }

    fn covering(&self, path: &str) -> Option<(&Arc<Filesystem>, String)> {
        let mount = self.mounts.get(self.covering_index(path)?)?;
        let tail = if mount.prefix == ROOT {
            String::from(path)
        } else {
            path.strip_prefix(mount.prefix.as_str())
                .filter(|rest| !rest.is_empty())
                .map_or_else(|| String::from(ROOT), String::from)
        };
        Some((&mount.filesystem, tail))
    }

    fn covering_index(&self, path: &str) -> Option<usize> {
        // Позиция различает записи, и два префикса одной файловой системы дают разные
        // монтирования.
        self.mounts
            .iter()
            .enumerate()
            .filter(|(_, mount)| under(mount.prefix.as_str(), path))
            .max_by_key(|(_, mount)| mount.prefix.len())
            .map(|(index, _)| index)
    }

    fn truncates_the_pair_reply(&self) {
        self.truncates_pair.store(true, Ordering::SeqCst);
    }

    fn answers_pair_root_len(&self, len: u32) {
        *self.pair_root_len.lock().expect("mutex not poisoned") = Some(len);
    }

    fn cap<K: ObjectKind>(&self) -> Cap<K> {
        let raw = self.next_cap.fetch_add(1, Ordering::SeqCst);
        Cap::from_handle(Handle::from_abi(
            core::num::NonZeroU32::new(raw).expect("capability id is non-zero"),
        ))
    }

    fn minted(&self) -> u32 {
        self.next_cap.load(Ordering::SeqCst) - 1
    }

    fn transports(&self) -> usize {
        self.transports.load(Ordering::SeqCst)
    }

    fn descriptors(&self) -> usize {
        self.live.load(Ordering::SeqCst)
    }

    fn resolves(&self) -> Vec<String> {
        self.resolved.lock().expect("mutex not poisoned").clone()
    }

    fn holds(&self, path: &str) -> bool {
        self.covering(path)
            .is_some_and(|(filesystem, tail)| filesystem.tree().contains_key(&tail))
    }

    fn session_end(self: &Arc<Self>, path: &str) -> MockChannel {
        let (filesystem, root) = self.covering(path).expect("a mount covers the path");
        self.session_end_in(Arc::clone(filesystem), root)
    }

    fn session_end_in(self: &Arc<Self>, filesystem: Arc<Filesystem>, root: String) -> MockChannel {
        let (client, server) = ipc_test::pair();
        let world = Arc::clone(self);
        thread::spawn(move || serve_session(&world, filesystem, root, &server));
        client
    }

    fn session(
        self: &Arc<Self>,
        filesystem: Arc<Filesystem>,
        root: String,
    ) -> ipc::wire::Session<file_system::Protocol> {
        let cap = self.cap();
        let end = self.session_end_in(filesystem, root);
        self.sessions
            .lock()
            .expect("mutex not poisoned")
            .insert(cap.raw().get(), end);
        ipc::wire::Session::from_handle(cap)
    }
}

struct TrackedEnd {
    end: MockChannel,
    world: Arc<World>,
}

impl TrackedEnd {
    fn new(world: &Arc<World>, end: MockChannel) -> Self {
        world.transports.fetch_add(1, Ordering::SeqCst);
        Self {
            end,
            world: Arc::clone(world),
        }
    }
}

impl Drop for TrackedEnd {
    fn drop(&mut self) {
        self.world.transports.fetch_sub(1, Ordering::SeqCst);
    }
}

impl Channel for TrackedEnd {
    type Resources = MockResources;

    fn send(&self, bytes: &[u8], handles: &[u32], wait: Timeout) -> Result<(), IpcError> {
        self.end.send(bytes, handles, wait)
    }

    fn call<'b>(
        &self,
        bytes: &[u8],
        handles: &[u32],
        reply: &'b mut FrameBytes,
        wait: Timeout,
    ) -> Result<Incoming<'b, MockResources>, IpcError> {
        let mut answer = self.end.call(bytes, handles, reply, wait)?;
        if self.world.claims_fail.load(Ordering::SeqCst) {
            // Ответ назвал capability, которой кадр не доставил.
            answer.resources = MockResources::default();
        }
        if self.world.truncates_pair.load(Ordering::SeqCst) {
            // Обрезанный после заголовка ответ не даёт декодировать агрегат пары.
            let len = answer.bytes.len().min(HEADER_SIZE + TRUNCATED_BODY);
            answer.bytes = &answer.bytes[..len];
        }
        Ok(answer)
    }
}

struct SessionServer {
    world: Arc<World>,
    filesystem: Arc<Filesystem>,
    root: String,
    buffer: Option<Shared>,
    open: BTreeMap<u32, String>,
    next_descriptor: u32,
}

impl SessionServer {
    fn root_session(
        &mut self,
        path: Str<'_, { PATH_MAX }>,
    ) -> Result<ipc::wire::Session<file_system::Protocol>, FileSystemError> {
        if self.world.scoping_fails.load(Ordering::SeqCst) {
            return Err(FileSystemError::NotFound);
        }
        let full = self.full(path.as_str());
        match self.filesystem.tree().get(&full) {
            Some(Node::Directory) => {}
            Some(Node::File(_)) => return Err(FileSystemError::NotADirectory),
            None => return Err(FileSystemError::NotFound),
        }
        Ok(self.world.session(Arc::clone(&self.filesystem), full))
    }

    fn buffer_capability(&mut self, size: u32) -> Cap<kind::MemoryRegion> {
        let shared: Shared = Arc::new(Mutex::new(std::vec![0; size as usize]));
        let cap = self.world.cap();
        self.world
            .buffers
            .lock()
            .expect("mutex not poisoned")
            .insert(cap.raw().get(), Arc::clone(&shared));
        self.buffer = Some(shared);
        cap
    }

    fn full(&self, path: &str) -> String {
        if self.root == ROOT {
            return String::from(path);
        }
        if path == ROOT {
            return self.root.clone();
        }
        let mut full = self.root.clone();
        full.push_str(path);
        full
    }

    fn path_of(&self, fd: NodeDescriptor) -> Result<String, FileSystemError> {
        self.open
            .get(&fd.raw())
            .cloned()
            .ok_or(FileSystemError::BadDescriptor)
    }

    fn transfer(&self) -> Result<(), FileSystemError> {
        let allowed = self.world.transfers_allowed.load(Ordering::SeqCst);
        if allowed == 0 {
            return Err(FileSystemError::Unresponsive);
        }
        if allowed != usize::MAX {
            self.world
                .transfers_allowed
                .store(allowed - 1, Ordering::SeqCst);
        }
        Ok(())
    }

    fn data(&self) -> Result<MutexGuard<'_, Vec<u8>>, FileSystemError> {
        Ok(self
            .buffer
            .as_ref()
            .ok_or(FileSystemError::NoBuffer)?
            .lock()
            .expect("mutex not poisoned"))
    }
}

impl<F: ipc::ReceivedResources> file_system::Service<F> for SessionServer {
    fn acquire_buffer(
        &mut self,
        _frame: &F,
        size: u32,
    ) -> impl ipc::IntoWireValue<Read = Result<Cap<kind::MemoryRegion>, FileSystemError>> {
        Ok::<_, FileSystemError>(self.buffer_capability(size))
    }

    fn open(
        &mut self,
        _frame: &F,
        path: Str<'_, { PATH_MAX }>,
        _mode: AccessMode,
        flags: OpenFlags,
    ) -> Result<Opened, FileSystemError> {
        let full = self.full(path.as_str());
        let mut tree = self.filesystem.tree();
        let node = if let Some(node) = tree.get(&full).cloned() {
            if flags.contains(OpenFlags::CREATE | OpenFlags::EXCLUSIVE) {
                return Err(FileSystemError::AlreadyExists);
            }
            if flags.contains(OpenFlags::DIRECTORY) && node != Node::Directory {
                return Err(FileSystemError::NotADirectory);
            }
            if flags.contains(OpenFlags::TRUNCATE) {
                tree.insert(full.clone(), Node::File(Vec::new()));
                Node::File(Vec::new())
            } else {
                node
            }
        } else {
            if !flags.contains(OpenFlags::CREATE) {
                return Err(FileSystemError::NotFound);
            }
            if tree.get(parent_of(&full)) != Some(&Node::Directory) {
                return Err(FileSystemError::NotFound);
            }
            let node = if flags.contains(OpenFlags::DIRECTORY) {
                Node::Directory
            } else {
                Node::File(Vec::new())
            };
            tree.insert(full.clone(), node.clone());
            node
        };
        let size = measure(&tree, &full, &node);
        drop(tree);
        let raw = self.next_descriptor;
        self.next_descriptor += 1;
        self.open.insert(raw, full);
        self.world.live.fetch_add(1, Ordering::SeqCst);
        Ok(Opened {
            fd: NodeDescriptor::from_raw(raw),
            kind: kind_of(&node),
            size,
        })
    }

    fn close(&mut self, _frame: &F, fd: NodeDescriptor) -> Result<u32, FileSystemError> {
        self.open
            .remove(&fd.raw())
            .ok_or(FileSystemError::BadDescriptor)?;
        self.world.live.fetch_sub(1, Ordering::SeqCst);
        Ok(0)
    }

    fn read(
        &mut self,
        _frame: &F,
        fd: NodeDescriptor,
        offset: u64,
        len: u32,
    ) -> Result<u32, FileSystemError> {
        self.world.reads.fetch_add(1, Ordering::SeqCst);
        self.transfer()?;
        let path = self.path_of(fd)?;
        let tree = self.filesystem.tree();
        let Some(Node::File(bytes)) = tree.get(&path) else {
            return Err(FileSystemError::IsADirectory);
        };
        let start = usize::try_from(offset)
            .unwrap_or(usize::MAX)
            .min(bytes.len());
        let end = start.saturating_add(len as usize).min(bytes.len());
        let slice = bytes[start..end].to_vec();
        drop(tree);
        let mut data = self.data()?;
        let head = data
            .get_mut(..slice.len())
            .ok_or(FileSystemError::BufferTooSmall)?;
        head.copy_from_slice(&slice);
        Ok(slice.len() as u32)
    }

    fn write(
        &mut self,
        _frame: &F,
        fd: NodeDescriptor,
        offset: u64,
        len: u32,
    ) -> Result<Written, FileSystemError> {
        self.transfer()?;
        let path = self.path_of(fd)?;
        let chunk = self
            .data()?
            .get(..len as usize)
            .ok_or(FileSystemError::BufferTooSmall)?
            .to_vec();
        let mut tree = self.filesystem.tree();
        let Some(Node::File(bytes)) = tree.get_mut(&path) else {
            return Err(FileSystemError::IsADirectory);
        };
        let at = if offset == APPEND_OFFSET {
            bytes.len()
        } else {
            usize::try_from(offset).unwrap_or(usize::MAX)
        };
        let end = at.saturating_add(chunk.len());
        if bytes.len() < end {
            bytes.resize(end, 0);
        }
        bytes[at..end].copy_from_slice(&chunk);
        Ok(Written {
            count: chunk.len() as u32,
            offset: at as u64,
        })
    }

    fn set_size(
        &mut self,
        _frame: &F,
        fd: NodeDescriptor,
        size: u64,
    ) -> Result<u32, FileSystemError> {
        let path = self.path_of(fd)?;
        let mut tree = self.filesystem.tree();
        let Some(Node::File(bytes)) = tree.get_mut(&path) else {
            return Err(FileSystemError::IsADirectory);
        };

        let size = usize::try_from(size).map_err(|_| FileSystemError::NoSpace)?;
        bytes.resize(size, 0);

        Ok(0)
    }

    fn stat(&mut self, _frame: &F, fd: NodeDescriptor) -> Result<NodeInfo, FileSystemError> {
        let path = self.path_of(fd)?;
        let tree = self.filesystem.tree();
        let node = tree.get(&path).ok_or(FileSystemError::NotFound)?.clone();
        Ok(NodeInfo {
            kind: kind_of(&node),
            size: measure(&tree, &path, &node),
        })
    }

    fn list(
        &mut self,
        _frame: &F,
        fd: NodeDescriptor,
        from: DirCursor,
    ) -> Result<Listed, FileSystemError> {
        let path = self.path_of(fd)?;
        let tree = self.filesystem.tree();
        if tree.get(&path) != Some(&Node::Directory) {
            return Err(FileSystemError::NotADirectory);
        }
        let children = children_of(&tree, &path);
        drop(tree);
        // Узел, подменённый между обходом и удалением, меняет вид ровно один раз.
        if let Some((swapped, node)) = self
            .world
            .swapped
            .lock()
            .expect("mutex not poisoned")
            .take()
        {
            self.filesystem.tree().insert(swapped, node);
        }
        let mut data = self.data()?;
        data.fill(0);
        let start = from.raw() as usize;
        let mut used = 0;
        let mut count = 0;
        for (kind, name) in children.iter().skip(start) {
            let entry = DirEntry {
                kind: *kind,
                name: name.as_str(),
            };
            let Ok(len) = entry.encode(&mut data[used..]) else {
                break;
            };
            used += len;
            count += 1;
        }
        let next = start + count;
        Ok(Listed {
            count: count as u32,
            next: DirCursor::from_raw(next as u32),
            end: next >= children.len(),
        })
    }

    fn rename(
        &mut self,
        _frame: &F,
        from: Str<'_, { PATH_MAX }>,
        to: Str<'_, { PATH_MAX }>,
    ) -> Result<u32, FileSystemError> {
        let from = self.full(from.as_str());
        let to = self.full(to.as_str());
        let mut tree = self.filesystem.tree();
        let node = tree.remove(&from).ok_or(FileSystemError::NotFound)?;
        tree.insert(to, node);
        Ok(0)
    }

    fn unlink(
        &mut self,
        _frame: &F,
        path: Str<'_, { PATH_MAX }>,
        expected: NodeKind,
    ) -> Result<u32, FileSystemError> {
        if path.as_str() == ROOT {
            return Err(FileSystemError::InvalidPath);
        }
        let full = self.full(path.as_str());
        if self
            .world
            .protected
            .lock()
            .expect("mutex not poisoned")
            .as_deref()
            == Some(full.as_str())
        {
            return Err(FileSystemError::ReadOnly);
        }
        let mut tree = self.filesystem.tree();
        let node = tree.get(&full).ok_or(FileSystemError::NotFound)?.clone();
        if kind_of(&node) != expected {
            return Err(match expected {
                NodeKind::File => FileSystemError::IsADirectory,
                NodeKind::Directory => FileSystemError::NotADirectory,
            });
        }
        if node == Node::Directory && !children_of(&tree, &full).is_empty() {
            return Err(FileSystemError::NotEmpty);
        }
        tree.remove(&full);
        Ok(0)
    }

    fn create_backed(
        &mut self,
        _frame: &F,
        _path: Str<'_, { PATH_MAX }>,
        _region: F::Handle<kind::MemoryRegion>,
        _size: u64,
    ) -> Result<u32, FileSystemError> {
        Err(FileSystemError::NotSupported)
    }

    fn backing_memory(
        &mut self,
        _frame: &F,
        _fd: NodeDescriptor,
    ) -> impl ipc::IntoWireValue<Read = Result<Cap<kind::MemoryRegion>, FileSystemError>> {
        Err::<Cap<kind::MemoryRegion>, _>(FileSystemError::NotSupported)
    }

    fn open_root(
        &mut self,
        _frame: &F,
        path: Str<'_, { PATH_MAX }>,
    ) -> impl ipc::IntoWireValue<Read = Result<ipc::wire::Session<file_system::Protocol>, FileSystemError>>
    {
        self.root_session(path)
    }
}

struct Router {
    world: Arc<World>,
}

impl Router {
    fn resolved_pair(
        &mut self,
        from: Str<'_, { PATH_MAX }>,
        to: Str<'_, { PATH_MAX }>,
    ) -> Result<ResolvedPair, FileSystemError> {
        let mut resolved = self.world.resolved.lock().expect("mutex not poisoned");
        resolved.push(String::from(from.as_str()));
        resolved.push(String::from(to.as_str()));
        drop(resolved);

        let first = self
            .world
            .covering_index(from.as_str())
            .ok_or(FileSystemError::NotFound)?;
        let second = self
            .world
            .covering_index(to.as_str())
            .ok_or(FileSystemError::NotFound)?;
        if first != second {
            return Err(FileSystemError::CrossMount);
        }

        let mount = self
            .world
            .mounts
            .get(first)
            .ok_or(FileSystemError::NotFound)?;
        let prefix = Path::new(&mount.prefix)?;
        // Общий каталог пары может лежать выше корня монтирования, и сессия не поднимается над ним.
        let common = common_directory(Path::new(from.as_str())?, Path::new(to.as_str())?)?;
        let scope = common.strip_prefix(prefix).map_or(prefix, |_| common);
        let tail = scope
            .strip_prefix(prefix)
            .ok_or(FileSystemError::NotFound)?;
        match mount.filesystem.tree().get(tail.as_str()) {
            Some(Node::Directory) => {}
            Some(Node::File(_)) => return Err(FileSystemError::NotADirectory),
            None => return Err(FileSystemError::NotFound),
        }

        let root_len = self
            .world
            .pair_root_len
            .lock()
            .expect("mutex not poisoned")
            .unwrap_or(scope.as_str().len() as u32);
        Ok(ResolvedPair {
            session: self
                .world
                .session(Arc::clone(&mount.filesystem), String::from(tail.as_str())),
            root_len,
        })
    }

    fn resolved_session(
        &mut self,
        path: Str<'_, { PATH_MAX }>,
    ) -> Result<ipc::wire::Session<file_system::Protocol>, FileSystemError> {
        self.world
            .resolved
            .lock()
            .expect("mutex not poisoned")
            .push(String::from(path.as_str()));
        let (filesystem, root) = self
            .world
            .covering(path.as_str())
            .ok_or(FileSystemError::NotFound)?;
        // Окно открывается на узле монтирования, и `open_root` не каталога даёт
        // `NotADirectory`.
        match filesystem.tree().get(&root) {
            Some(Node::Directory) => {}
            Some(Node::File(_)) => return Err(FileSystemError::NotADirectory),
            None => return Err(FileSystemError::NotFound),
        }
        Ok(self.world.session(Arc::clone(filesystem), root))
    }
}

impl<F: ipc::ReceivedResources> namespace::Service<F> for Router {
    fn resolve(
        &mut self,
        _frame: &F,
        path: Str<'_, { PATH_MAX }>,
        reply: namespace::resolve::Reply<F::Reply>,
    ) {
        let _ = reply.send(self.resolved_session(path));
    }

    fn resolve_pair(
        &mut self,
        _frame: &F,
        from: Str<'_, { PATH_MAX }>,
        to: Str<'_, { PATH_MAX }>,
        reply: namespace::resolve_pair::Reply<F::Reply>,
    ) {
        let _ = reply.send(self.resolved_pair(from, to));
    }
}

struct TestEnvironment {
    world: Arc<World>,
}

impl Environment for TestEnvironment {
    type Handle<K: ObjectKind> = Cap<K>;
    type Transport = TrackedEnd;
    type Region = TestBuffer;

    fn connect(
        &self,
        session: ipc::wire::Session<file_system::Protocol>,
    ) -> Result<TrackedEnd, FileSystemError> {
        let end = self
            .world
            .sessions
            .lock()
            .expect("mutex not poisoned")
            .remove(&session.into_handle().raw().get())
            .ok_or(FileSystemError::NoSession)?;
        Ok(TrackedEnd::new(&self.world, end))
    }

    fn map(&self, region: Cap<kind::MemoryRegion>) -> Result<TestBuffer, FileSystemError> {
        if self.world.mappings_allowed.load(Ordering::SeqCst) == 0 {
            return Err(FileSystemError::NoSpace);
        }
        self.world.mappings_allowed.fetch_sub(1, Ordering::SeqCst);
        let bytes = self
            .world
            .buffers
            .lock()
            .expect("mutex not poisoned")
            .get(&region.raw().get())
            .cloned()
            .ok_or(FileSystemError::NoBuffer)?;
        Ok(TestBuffer { bytes })
    }
}

struct TestBuffer {
    bytes: Shared,
}

impl SharedRegion for TestBuffer {
    fn size(&self) -> usize {
        self.bytes.lock().expect("mutex not poisoned").len()
    }

    fn read_at(&self, offset: usize, dst: &mut [u8]) -> Result<(), FileSystemError> {
        let bytes = self.bytes.lock().expect("mutex not poisoned");
        let end = offset.saturating_add(dst.len());
        let head = bytes
            .get(offset..end)
            .ok_or(FileSystemError::BufferTooSmall)?;
        dst.copy_from_slice(head);
        Ok(())
    }

    fn write_at(&self, offset: usize, src: &[u8]) -> Result<(), FileSystemError> {
        let mut bytes = self.bytes.lock().expect("mutex not poisoned");
        let end = offset.saturating_add(src.len());
        let head = bytes
            .get_mut(offset..end)
            .ok_or(FileSystemError::BufferTooSmall)?;
        head.copy_from_slice(src);
        Ok(())
    }
}

fn serve_session(
    world: &Arc<World>,
    filesystem: Arc<Filesystem>,
    root: String,
    end: &MockListener,
) {
    let mut server = SessionServer {
        world: Arc::clone(world),
        filesystem,
        root,
        buffer: None,
        open: BTreeMap::new(),
        next_descriptor: 1,
    };
    let mut bytes = [0u8; MESSAGE_INLINE_MAX];
    while let Ok(frame) = end.recv(&mut bytes, Timeout::INFINITE) {
        if world.poison.swap(false, Ordering::SeqCst) {
            return;
        }
        if file_system::dispatch(&mut server, frame).is_err() {
            return;
        }
    }
}

fn under(prefix: &str, path: &str) -> bool {
    if prefix == ROOT {
        return true;
    }
    path.strip_prefix(prefix)
        .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
}

fn parent_of(path: &str) -> &str {
    match path.rfind('/') {
        Some(0) | None => ROOT,
        Some(cut) => &path[..cut],
    }
}

fn kind_of(node: &Node) -> NodeKind {
    match node {
        Node::File(_) => NodeKind::File,
        Node::Directory => NodeKind::Directory,
    }
}

fn measure(tree: &BTreeMap<String, Node>, path: &str, node: &Node) -> u64 {
    match node {
        Node::File(bytes) => bytes.len() as u64,
        Node::Directory => children_of(tree, path).len() as u64,
    }
}

fn children_of(tree: &BTreeMap<String, Node>, directory: &str) -> Vec<(NodeKind, String)> {
    let mut prefix = String::from(directory);
    if directory != ROOT {
        prefix.push('/');
    }
    tree.iter()
        .filter_map(|(path, node)| {
            let rest = path.strip_prefix(prefix.as_str())?;
            if rest.is_empty() || rest.contains('/') {
                return None;
            }
            Some((kind_of(node), String::from(rest)))
        })
        .collect()
}

fn file(text: &str) -> Node {
    Node::File(text.as_bytes().to_vec())
}

fn routed(world: &Arc<World>) -> FileSystem<TestEnvironment> {
    let (client, server) = ipc_test::pair();
    let mut router = Router {
        world: Arc::clone(world),
    };
    thread::spawn(move || {
        let _ = server.serve_all(|frame| namespace::dispatch(&mut router, frame));
    });
    FileSystem::routed_with_environment(
        TestEnvironment {
            world: Arc::clone(world),
        },
        TrackedEnd::new(world, client),
    )
}

fn windowed(world: &Arc<World>, root: &str) -> FileSystem<TestEnvironment> {
    let end = world.session_end(root);
    FileSystem::windowed_with_environment(
        TestEnvironment {
            world: Arc::clone(world),
        },
        TrackedEnd::new(world, end),
    )
}

fn path(text: &str) -> Path<'_> {
    Path::new(text).expect("path within the contract")
}

fn read_all(filesystem: &mut FileSystem<TestEnvironment>, text: &str, len: usize) -> Vec<u8> {
    let mut node = filesystem
        .open(path(text), AccessMode::Read, OpenFlags::NONE)
        .expect("open");
    let mut bytes = std::vec![0; len];
    let count = node.read(&mut bytes).expect("read");
    bytes.truncate(count);
    bytes
}

#[test]
fn second_open_in_the_same_directory_keeps_the_session() {
    let world = World::new(&[
        ("/data", Node::Directory),
        ("/data/first", file("one")),
        ("/data/second", file("two")),
    ]);
    let mut filesystem = routed(&world);

    assert_eq!(read_all(&mut filesystem, "/data/first", 8), b"one");
    assert_eq!(read_all(&mut filesystem, "/data/second", 8), b"two");

    assert_eq!(world.resolves(), [String::from("/data")]);
}

#[test]
fn missing_name_is_rechecked_through_the_router() {
    let world = World::new(&[("/data", Node::Directory), ("/data/first", file("one"))]);
    let mut filesystem = routed(&world);
    assert_eq!(read_all(&mut filesystem, "/data/first", 8), b"one");

    let outcome = filesystem.open(path("/data/absent"), AccessMode::Read, OpenFlags::NONE);

    assert!(matches!(
        outcome.err(),
        Some(Failure::FileSystem(FileSystemError::NotFound))
    ));
    assert_eq!(
        world.resolves(),
        [
            String::from("/data"),
            String::from("/data"),
            String::from("/data/absent"),
        ]
    );
}

#[test]
fn session_is_reopened_after_the_provider_leaves() {
    let world = World::new(&[("/data", Node::Directory), ("/data/first", file("one"))]);
    let mut filesystem = routed(&world);
    assert_eq!(read_all(&mut filesystem, "/data/first", 8), b"one");

    world.poison.store(true, Ordering::SeqCst);

    assert_eq!(read_all(&mut filesystem, "/data/first", 8), b"one");
    assert_eq!(
        world.resolves(),
        [String::from("/data"), String::from("/data")]
    );
}

#[test]
fn the_second_refusal_reaches_the_caller() {
    let world = World::new(&[("/data", Node::Directory), ("/data/first", file("one"))]);
    let mut filesystem = routed(&world);
    assert_eq!(read_all(&mut filesystem, "/data/first", 8), b"one");

    world.poison.store(true, Ordering::SeqCst);
    world.tree().remove("/data/first");
    world.tree().remove("/data");

    let outcome = filesystem.open(path("/data/first"), AccessMode::Read, OpenFlags::NONE);

    assert!(matches!(
        outcome.err(),
        Some(Failure::FileSystem(FileSystemError::NotFound))
    ));
    assert_eq!(
        world.resolves(),
        [
            String::from("/data"),
            String::from("/data"),
            String::from("/data/first"),
        ]
    );
}

#[test]
fn two_threads_read_their_own_data_through_one_session() {
    const ROUNDS: usize = 64;

    let first = "a".repeat(100);
    let second = "b".repeat(100);
    let world = World::new(&[
        ("/data", Node::Directory),
        ("/data/first", file(&first)),
        ("/data/second", file(&second)),
    ]);
    let filesystem = Arc::new(Mutex::new(routed(&world)));

    thread::scope(|scope| {
        for (name, body) in [("/data/first", &first), ("/data/second", &second)] {
            let filesystem = Arc::clone(&filesystem);
            scope.spawn(move || {
                for _ in 0..ROUNDS {
                    let mut guard = filesystem.lock().expect("mutex not poisoned");
                    assert_eq!(read_all(&mut guard, name, 200), body.as_bytes());
                }
            });
        }
    });

    assert_eq!(world.resolves(), [String::from("/data")]);
}

#[test]
fn descriptor_closes_with_the_node() {
    let world = World::new(&[("/data", Node::Directory), ("/data/first", file("one"))]);
    let mut filesystem = routed(&world);

    let node = filesystem
        .open(path("/data/first"), AccessMode::Read, OpenFlags::NONE)
        .expect("open");
    assert_eq!(world.live.load(Ordering::SeqCst), 1);
    drop(node);

    assert_eq!(world.live.load(Ordering::SeqCst), 0);
}

#[test]
fn a_mapping_failure_releases_the_session_transport() {
    let world = World::new(&[("/data", Node::Directory), ("/data/first", file("one"))]);
    world.mappings_allowed.store(0, Ordering::SeqCst);
    let mut filesystem = windowed(&world, "/data");

    let outcome = filesystem.open(path("/first"), AccessMode::Read, OpenFlags::NONE);

    assert!(matches!(
        outcome.err(),
        Some(Failure::FileSystem(FileSystemError::NoSpace))
    ));
    // Буфер сессии не отобразился, поэтому её транспорт закрыт вместе с попыткой, а
    // дескрипторов она не открыла.
    assert_eq!(world.transports.load(Ordering::SeqCst), 0);
    assert_eq!(world.live.load(Ordering::SeqCst), 0);
}

#[test]
fn a_buffer_capability_the_reply_did_not_carry_is_refused() {
    let world = World::new(&[("/data", Node::Directory), ("/data/first", file("one"))]);
    world.claims_fail.store(true, Ordering::SeqCst);
    let mut filesystem = windowed(&world, "/data");

    let outcome = filesystem.open(path("/first"), AccessMode::Read, OpenFlags::NONE);

    assert_eq!(
        outcome.err(),
        Some(Failure::Transport(IpcError::InvalidValue))
    );
    assert_eq!(world.transports(), 0);
    assert_eq!(world.descriptors(), 0);
}

#[test]
fn a_scope_capability_the_reply_did_not_carry_is_refused() {
    let world = World::new(&[("/data", Node::Directory), ("/data/first", file("one"))]);
    let mut filesystem = routed(&world);
    assert_eq!(read_all(&mut filesystem, "/data/first", 8), b"one");
    let idle = world.transports();
    world.claims_fail.store(true, Ordering::SeqCst);

    let outcome = filesystem.open(path("/data/first"), AccessMode::Read, OpenFlags::NONE);

    assert_eq!(
        outcome.err(),
        Some(Failure::Transport(IpcError::InvalidValue))
    );
    assert_eq!(world.transports(), idle);
    assert_eq!(world.descriptors(), 0);
}

#[test]
fn sequential_reads_continue_from_the_position() {
    let world = World::new(&[("/data", Node::Directory), ("/data/first", file("abcdef"))]);
    let mut filesystem = routed(&world);
    let mut node = filesystem
        .open(path("/data/first"), AccessMode::Read, OpenFlags::NONE)
        .expect("open");

    let mut head = [0u8; 3];
    let mut tail = [0u8; 3];
    assert_eq!(node.read(&mut head).expect("first read"), 3);
    assert_eq!(node.read(&mut tail).expect("second read"), 3);

    assert_eq!(&head, b"abc");
    assert_eq!(&tail, b"def");
    assert_eq!(node.position(), 6);
}

#[test]
fn read_beyond_the_buffer_goes_in_parts() {
    let body = "c".repeat(BUFFER_SIZE as usize * 3);
    let world = World::new(&[("/data", Node::Directory), ("/data/first", file(&body))]);
    let mut filesystem = routed(&world);

    let read = read_all(&mut filesystem, "/data/first", body.len());

    assert_eq!(read, body.as_bytes());
    assert!(world.reads.load(Ordering::SeqCst) >= 3);
}

#[test]
fn write_beyond_the_buffer_goes_in_parts() {
    let body = "d".repeat(BUFFER_SIZE as usize * 3 + 7);
    let world = World::new(&[("/data", Node::Directory), ("/data/first", file(""))]);
    let mut filesystem = routed(&world);

    let mut node = filesystem
        .open(path("/data/first"), AccessMode::ReadWrite, OpenFlags::NONE)
        .expect("open");
    assert_eq!(node.write(body.as_bytes()).expect("write"), body.len());
    assert_eq!(node.position(), body.len() as u64);
    drop(node);

    assert_eq!(
        read_all(&mut filesystem, "/data/first", body.len()),
        body.as_bytes()
    );
}

// Отказ файлового сервиса на второй порции не скрывает уже прочитанные байты и сдвиг
// позиции.
#[test]
fn a_refused_second_read_part_returns_the_first() {
    let body = "f".repeat(BUFFER_SIZE as usize * 2);
    let world = World::new(&[("/data", Node::Directory), ("/data/first", file(&body))]);
    let mut filesystem = routed(&world);
    let mut node = filesystem
        .open(path("/data/first"), AccessMode::Read, OpenFlags::NONE)
        .expect("open");
    world.transfers_allowed.store(1, Ordering::SeqCst);
    let mut bytes = std::vec![0; body.len()];

    assert_eq!(node.read(&mut bytes).expect("read"), BUFFER_SIZE as usize);
    assert_eq!(node.position(), u64::from(BUFFER_SIZE));
    assert_eq!(
        node.read(&mut bytes).err(),
        Some(Failure::FileSystem(FileSystemError::Unresponsive))
    );
    assert_eq!(node.position(), u64::from(BUFFER_SIZE));
}

// Отказ файлового сервиса на второй порции не скрывает уже записанные байты и сдвиг
// позиции.
#[test]
fn a_refused_second_write_part_returns_the_first() {
    let body = "g".repeat(BUFFER_SIZE as usize * 2);
    let world = World::new(&[("/data", Node::Directory), ("/data/first", file(""))]);
    let mut filesystem = routed(&world);
    let mut node = filesystem
        .open(path("/data/first"), AccessMode::ReadWrite, OpenFlags::NONE)
        .expect("open");
    world.transfers_allowed.store(1, Ordering::SeqCst);

    assert_eq!(
        node.write(body.as_bytes()).expect("write"),
        BUFFER_SIZE as usize
    );
    assert_eq!(node.position(), u64::from(BUFFER_SIZE));
    assert_eq!(
        node.write(body.as_bytes()).err(),
        Some(Failure::FileSystem(FileSystemError::Unresponsive))
    );
    assert_eq!(node.position(), u64::from(BUFFER_SIZE));
    drop(node);
    world.transfers_allowed.store(usize::MAX, Ordering::SeqCst);

    assert_eq!(
        read_all(&mut filesystem, "/data/first", body.len()),
        &body.as_bytes()[..BUFFER_SIZE as usize]
    );
}

#[test]
fn append_lands_at_the_end_of_the_node() {
    let world = World::new(&[("/data", Node::Directory), ("/data/first", file("one"))]);
    let mut filesystem = routed(&world);
    let node = filesystem
        .open(path("/data/first"), AccessMode::Write, OpenFlags::NONE)
        .expect("open");

    let written = node.append(b"two").expect("append");

    assert_eq!(
        written,
        Written {
            count: 3,
            offset: 3,
        }
    );
    drop(node);
    assert_eq!(read_all(&mut filesystem, "/data/first", 16), b"onetwo");
}

#[test]
fn append_beyond_the_buffer_writes_the_fitting_prefix() {
    let body = "e".repeat(BUFFER_SIZE as usize + 1);
    let world = World::new(&[("/data", Node::Directory), ("/data/first", file("one"))]);
    let mut filesystem = routed(&world);
    let node = filesystem
        .open(path("/data/first"), AccessMode::Write, OpenFlags::NONE)
        .expect("open");

    // Длинный срез даёт короткий успех одной файловой операции, а не цепочку независимо
    // неделимых дозаписей.
    let written = node.append(body.as_bytes()).expect("append");

    assert_eq!(
        written,
        Written {
            count: BUFFER_SIZE,
            offset: 3,
        }
    );
    drop(node);
    assert_eq!(
        read_all(&mut filesystem, "/data/first", 3 + body.len()).len(),
        3 + BUFFER_SIZE as usize
    );
}

#[test]
fn set_size_cuts_and_extends_the_node_and_keeps_the_position() {
    let world = World::new(&[("/data", Node::Directory), ("/data/first", file("abcdef"))]);
    let mut filesystem = routed(&world);
    let mut node = filesystem
        .open(path("/data/first"), AccessMode::ReadWrite, OpenFlags::NONE)
        .expect("open");
    node.seek(4);

    node.set_size(2).expect("cut");
    assert_eq!(node.metadata().expect("stat").size, 2);
    node.set_size(4).expect("extend");

    assert_eq!(node.position(), 4);
    drop(node);
    assert_eq!(read_all(&mut filesystem, "/data/first", 8), b"ab\0\0");
}

#[test]
fn set_size_of_a_directory_is_refused() {
    let world = World::new(&[("/data", Node::Directory)]);
    let mut filesystem = routed(&world);
    let node = filesystem
        .open(path("/data"), AccessMode::Read, OpenFlags::DIRECTORY)
        .expect("open");

    assert_eq!(
        node.set_size(0),
        Err(Failure::FileSystem(FileSystemError::IsADirectory))
    );
}

#[test]
fn an_open_node_outlives_the_next_facade_call() {
    let world = World::new(&[
        ("/data", Node::Directory),
        ("/data/first", file("one")),
        ("/data/second", file("two")),
    ]);
    let mut filesystem = routed(&world);

    let mut node = filesystem
        .open(path("/data/first"), AccessMode::Read, OpenFlags::NONE)
        .expect("open");
    assert_eq!(read_all(&mut filesystem, "/data/second", 8), b"two");

    let mut bytes = [0u8; 8];
    let count = node.read(&mut bytes).expect("read");

    assert_eq!(&bytes[..count], b"one");
    drop(node);
    assert_eq!(world.descriptors(), 0);
}

#[test]
fn two_nodes_of_one_directory_own_separate_sessions() {
    let world = World::new(&[
        ("/data", Node::Directory),
        ("/data/first", file("one")),
        ("/data/second", file("two")),
    ]);
    let mut filesystem = routed(&world);
    // Кэш каталога уже заполнен, и дальше растёт только число производных сессий.
    assert_eq!(read_all(&mut filesystem, "/data/first", 8), b"one");
    let idle = world.transports();

    let mut first = filesystem
        .open(path("/data/first"), AccessMode::Read, OpenFlags::NONE)
        .expect("first open");
    let mut second = filesystem
        .open(path("/data/second"), AccessMode::Read, OpenFlags::NONE)
        .expect("second open");

    assert_eq!(world.transports(), idle + 2);
    let mut bytes = [0u8; 8];
    let count = first.read(&mut bytes).expect("first read");
    assert_eq!(&bytes[..count], b"one");
    let count = second.read(&mut bytes).expect("second read");
    assert_eq!(&bytes[..count], b"two");

    drop(first);
    drop(second);
    assert_eq!(world.transports(), idle);
    assert_eq!(world.descriptors(), 0);
    assert_eq!(world.resolves(), [String::from("/data")]);
}

#[test]
fn a_refused_scope_leaves_no_session_behind() {
    let world = World::new(&[("/data", Node::Directory), ("/data/first", file("one"))]);
    let mut filesystem = routed(&world);
    assert_eq!(read_all(&mut filesystem, "/data/first", 8), b"one");
    let idle = world.transports();
    world.scoping_fails.store(true, Ordering::SeqCst);

    let outcome = filesystem.open(path("/data/first"), AccessMode::Read, OpenFlags::NONE);

    assert!(outcome.is_err());
    assert_eq!(world.transports(), idle);
    assert_eq!(world.descriptors(), 0);
}

#[test]
fn a_refused_buffer_of_the_scoped_session_leaves_no_session_behind() {
    let world = World::new(&[("/data", Node::Directory), ("/data/first", file("one"))]);
    let mut filesystem = routed(&world);
    assert_eq!(read_all(&mut filesystem, "/data/first", 8), b"one");
    let idle = world.transports();
    world.mappings_allowed.store(0, Ordering::SeqCst);

    let outcome = filesystem.open(path("/data/first"), AccessMode::Read, OpenFlags::NONE);

    assert!(matches!(
        outcome.err(),
        Some(Failure::FileSystem(FileSystemError::NoSpace))
    ));
    assert_eq!(world.transports(), idle);
    assert_eq!(world.descriptors(), 0);
}

#[test]
fn a_refused_descriptor_of_the_scoped_session_leaves_no_session_behind() {
    let world = World::new(&[("/data", Node::Directory), ("/data/first", file("one"))]);
    let mut filesystem = routed(&world);
    assert_eq!(read_all(&mut filesystem, "/data/first", 8), b"one");
    let idle = world.transports();

    let outcome = filesystem.open(path("/data/absent"), AccessMode::Read, OpenFlags::NONE);

    assert!(matches!(
        outcome.err(),
        Some(Failure::FileSystem(FileSystemError::NotFound))
    ));
    assert_eq!(world.transports(), idle);
    assert_eq!(world.descriptors(), 0);
}

#[test]
fn a_window_facade_keeps_its_capability_after_a_refused_scope() {
    let world = World::new(&[("/data", Node::Directory), ("/data/first", file("one"))]);
    let mut filesystem = windowed(&world, "/data");
    assert_eq!(read_all(&mut filesystem, "/first", 8), b"one");
    let idle = world.transports();
    world.scoping_fails.store(true, Ordering::SeqCst);

    let outcome = filesystem.open(path("/first"), AccessMode::Read, OpenFlags::NONE);

    assert!(matches!(
        outcome.err(),
        Some(Failure::FileSystem(FileSystemError::NotFound))
    ));
    // Выданное окно не заменяется, потому что у него нет маршрутизатора, а его сессия
    // действует.
    assert!(world.resolves().is_empty());
    assert_eq!(world.transports(), idle);
    world.scoping_fails.store(false, Ordering::SeqCst);
    assert_eq!(read_all(&mut filesystem, "/first", 8), b"one");
}

#[test]
fn metadata_reports_the_kind_and_the_size() {
    let world = World::new(&[("/data", Node::Directory), ("/data/first", file("one"))]);
    let mut filesystem = routed(&world);

    assert_eq!(
        filesystem.metadata(path("/data/first")).expect("metadata"),
        NodeInfo {
            kind: NodeKind::File,
            size: 3,
        }
    );
    assert_eq!(
        filesystem.metadata(path("/data")).expect("metadata"),
        NodeInfo {
            kind: NodeKind::Directory,
            size: 1,
        }
    );
}

#[test]
fn listing_spans_several_buffer_fills() {
    const ENTRIES: usize = 500;

    let mut seed = std::vec![(String::from("/data"), Node::Directory)];
    for index in 0..ENTRIES {
        seed.push((std::format!("/data/entry{index:03}"), Node::Directory));
    }
    let world = World::new(&[]);
    for (path, node) in seed {
        world.tree().insert(path, node);
    }
    let mut filesystem = routed(&world);

    let mut listing = filesystem.read_directory(path("/data")).expect("listing");
    let mut names = Vec::new();
    while let Some(entry) = listing.next_entry().expect("entry") {
        names.push(String::from(entry.name));
    }

    assert_eq!(names.len(), ENTRIES);
    assert!(names.contains(&String::from("entry000")));
    assert!(names.contains(&String::from("entry059")));
}

#[test]
fn a_listing_outlives_the_next_facade_call() {
    let world = World::new(&[
        ("/data", Node::Directory),
        ("/data/first", file("one")),
        ("/data/second", file("two")),
    ]);
    let mut filesystem = routed(&world);

    let mut listing = filesystem.read_directory(path("/data")).expect("listing");
    // Фасад продолжает работать, потому что сессия обхода ему не принадлежит.
    assert_eq!(read_all(&mut filesystem, "/data/first", 8), b"one");
    let held = world.transports();

    let mut names = Vec::new();
    while let Some(entry) = listing.next_entry().expect("entry") {
        names.push(String::from(entry.name));
    }

    assert_eq!(names, [String::from("first"), String::from("second")]);
    drop(listing);
    assert_eq!(world.transports(), held - 1);
    assert_eq!(world.descriptors(), 0);
}

#[test]
fn an_unfinished_listing_closes_its_directory() {
    let world = World::new(&[
        ("/data", Node::Directory),
        ("/data/first", file("one")),
        ("/data/second", file("two")),
    ]);
    let mut filesystem = routed(&world);

    let mut listing = filesystem.read_directory(path("/data")).expect("listing");
    assert!(listing.next_entry().expect("entry").is_some());
    let held = world.transports();
    drop(listing);

    assert_eq!(world.transports(), held - 1);
    assert_eq!(world.descriptors(), 0);
}

#[test]
fn empty_directory_yields_no_entries() {
    let world = World::new(&[("/data", Node::Directory)]);
    let mut filesystem = routed(&world);

    let mut listing = filesystem.read_directory(path("/data")).expect("listing");

    assert!(listing.next_entry().expect("entry").is_none());
}

#[test]
fn nodes_are_created_removed_and_renamed() {
    let world = World::new(&[("/data", Node::Directory)]);
    let mut filesystem = routed(&world);

    let node = filesystem
        .open(
            path("/data/first"),
            AccessMode::ReadWrite,
            OpenFlags::CREATE,
        )
        .expect("create");
    drop(node);
    filesystem
        .create_directory(path("/data/nested"))
        .expect("mkdir");
    assert!(world.holds("/data/first"));
    assert!(world.holds("/data/nested"));

    filesystem
        .rename(path("/data/first"), path("/data/second"))
        .expect("rename");
    assert!(!world.holds("/data/first"));
    assert!(world.holds("/data/second"));

    filesystem
        .remove_file(path("/data/second"))
        .expect("remove");
    filesystem
        .remove_directory(path("/data/nested"))
        .expect("remove");
    assert!(!world.holds("/data/second"));
    assert!(!world.holds("/data/nested"));
}

#[test]
fn remove_tree_takes_the_whole_subtree() {
    let world = World::new(&[
        ("/data", Node::Directory),
        ("/data/tree", Node::Directory),
        ("/data/tree/first", file("one")),
        ("/data/tree/nested", Node::Directory),
        ("/data/tree/nested/second", file("two")),
        ("/data/keep", file("kept")),
    ]);
    let mut filesystem = routed(&world);

    filesystem
        .remove_tree(path("/data/tree"))
        .expect("remove tree");

    assert!(!world.holds("/data/tree"));
    assert!(!world.holds("/data/tree/nested/second"));
    assert!(world.holds("/data/keep"));
}

#[test]
fn removal_requires_the_expected_kind() {
    let world = World::new(&[
        ("/data", Node::Directory),
        ("/data/nested", Node::Directory),
        ("/data/nested/inner", file("two")),
        ("/data/first", file("one")),
    ]);
    let mut filesystem = routed(&world);

    let directory_as_file = filesystem.remove_file(path("/data/nested"));
    let file_as_directory = filesystem.remove_directory(path("/data/first"));

    assert!(matches!(
        directory_as_file.err(),
        Some(Failure::FileSystem(FileSystemError::IsADirectory))
    ));
    assert!(matches!(
        file_as_directory.err(),
        Some(Failure::FileSystem(FileSystemError::NotADirectory))
    ));
    assert!(world.holds("/data/nested"));
    assert!(world.holds("/data/nested/inner"));
    assert!(world.holds("/data/first"));
}

#[test]
fn remove_tree_refuses_a_child_whose_kind_changed() {
    let world = World::new(&[
        ("/data", Node::Directory),
        ("/data/tree", Node::Directory),
        ("/data/tree/child", file("one")),
    ]);
    *world.swapped.lock().expect("mutex not poisoned") =
        Some((String::from("/data/tree/child"), Node::Directory));
    let mut filesystem = routed(&world);

    let outcome = filesystem.remove_tree(path("/data/tree"));

    assert!(matches!(
        outcome.err(),
        Some(Failure::FileSystem(FileSystemError::IsADirectory))
    ));
    assert!(world.holds("/data/tree/child"));
    assert!(world.holds("/data/tree"));
}

#[test]
fn remove_tree_stops_at_the_first_refusal() {
    let world = World::new(&[
        ("/data", Node::Directory),
        ("/data/tree", Node::Directory),
        ("/data/tree/nested", Node::Directory),
        ("/data/tree/nested/second", file("two")),
    ]);
    *world.protected.lock().expect("mutex not poisoned") =
        Some(String::from("/data/tree/nested/second"));
    let mut filesystem = routed(&world);

    let outcome = filesystem.remove_tree(path("/data/tree"));

    assert!(matches!(
        outcome.err(),
        Some(Failure::FileSystem(FileSystemError::ReadOnly))
    ));
    assert!(world.holds("/data/tree/nested/second"));
    assert!(world.holds("/data/tree"));
}

#[test]
fn a_path_of_a_wrong_form_never_reaches_the_router() {
    let world = World::new(&[("/data", Node::Directory)]);
    let _filesystem = routed(&world);
    let long_path = std::format!("/{}", "p".repeat(PATH_MAX));
    let long_name = std::format!("/data/{}", "p".repeat(NAME_MAX + 1));

    for text in [long_path.as_str(), long_name.as_str(), "data/first"] {
        assert_eq!(Path::new(text), Err(FileSystemError::InvalidPath));
    }

    assert!(world.resolves().is_empty());
}

#[test]
fn a_window_session_resolves_from_its_own_root() {
    let world = World::new(&[("/data", Node::Directory), ("/data/first", file("one"))]);
    let mut filesystem = windowed(&world, "/data");

    assert_eq!(read_all(&mut filesystem, "/first", 8), b"one");
    assert!(world.resolves().is_empty());
}

#[test]
fn a_mount_root_is_read_through_its_own_filesystem() {
    let world = World::mounted(&[
        (ROOT, &[("/boot", Node::Directory)]),
        (
            "/tmp",
            &[("/first", file("one")), ("/nested", Node::Directory)],
        ),
    ]);
    let mut filesystem = routed(&world);

    let mut listing = filesystem.read_directory(path("/tmp")).expect("listing");
    let mut names = Vec::new();
    while let Some(entry) = listing.next_entry().expect("entry") {
        names.push(String::from(entry.name));
    }
    drop(listing);

    assert_eq!(names, [String::from("first"), String::from("nested")]);
    assert_eq!(
        filesystem.metadata(path("/tmp")).expect("metadata"),
        NodeInfo {
            kind: NodeKind::Directory,
            size: 2,
        }
    );
    assert_eq!(world.resolves().first(), Some(&String::from(ROOT)));
    assert!(world.resolves().contains(&String::from("/tmp")));
}

#[test]
fn a_node_under_a_mount_root_keeps_the_short_route() {
    let world = World::mounted(&[
        (ROOT, &[]),
        ("/tmp", &[("/first", file("one")), ("/second", file("two"))]),
    ]);
    let mut filesystem = routed(&world);

    assert_eq!(read_all(&mut filesystem, "/tmp/first", 8), b"one");
    assert_eq!(read_all(&mut filesystem, "/tmp/second", 8), b"two");

    assert_eq!(world.resolves(), [String::from("/tmp")]);
}

#[test]
fn a_mount_root_without_a_parent_directory_is_reachable() {
    let world = World::mounted(&[(ROOT, &[]), ("/store/area", &[("/first", file("one"))])]);
    let mut filesystem = routed(&world);

    assert_eq!(read_all(&mut filesystem, "/store/area/first", 8), b"one");
    assert_eq!(
        filesystem.metadata(path("/store/area")).expect("metadata"),
        NodeInfo {
            kind: NodeKind::Directory,
            size: 1,
        }
    );
}

#[test]
fn removing_a_mount_root_reaches_the_mounted_filesystem() {
    let world = World::mounted(&[(ROOT, &[]), ("/tmp", &[("/first", file("one"))])]);
    let mut filesystem = routed(&world);

    let outcome = filesystem.remove_directory(path("/tmp"));

    assert!(matches!(
        outcome.err(),
        Some(Failure::FileSystem(FileSystemError::InvalidPath))
    ));
    assert!(world.holds("/tmp/first"));
}

#[test]
fn remove_tree_refuses_the_session_root() {
    let world = World::mounted(&[(ROOT, &[]), ("/tmp", &[("/first", file("one"))])]);
    let mut filesystem = routed(&world);

    let outcome = filesystem.remove_tree(path("/tmp"));

    assert!(matches!(
        outcome.err(),
        Some(Failure::FileSystem(FileSystemError::InvalidPath))
    ));
    assert!(world.holds("/tmp/first"));

    let mut window = windowed(&world, "/tmp");
    assert!(matches!(
        window.remove_tree(path(ROOT)).err(),
        Some(Failure::FileSystem(FileSystemError::InvalidPath))
    ));
    assert!(world.holds("/tmp/first"));
}

#[test]
fn rename_between_directories_runs_on_their_common_root() {
    let world = World::new(&[
        ("/data", Node::Directory),
        ("/data/left", Node::Directory),
        ("/data/left/first", file("one")),
        ("/data/right", Node::Directory),
    ]);
    let mut filesystem = routed(&world);

    filesystem
        .rename(path("/data/left/first"), path("/data/right/second"))
        .expect("rename");

    assert!(!world.holds("/data/left/first"));
    assert!(world.holds("/data/right/second"));
    // Обе стороны уходят маршрутизатору одним решением, а не общим предком, вычисленным заранее.
    assert_eq!(
        world.resolves(),
        [
            String::from("/data/left/first"),
            String::from("/data/right/second")
        ]
    );
}

#[test]
fn rename_across_mounts_leaves_the_parent_filesystem_untouched() {
    // Общий предок пары лежит над монтированием, и разбор по нему увёл бы `rename` в родительскую
    // файловую систему с одноимёнными узлами.
    let world = World::mounted(&[
        (
            ROOT,
            &[
                ("/data", Node::Directory),
                ("/data/left", Node::Directory),
                ("/data/left/first", file("parent")),
                ("/data/right", Node::Directory),
            ],
        ),
        ("/data/left", &[("/first", file("mounted"))]),
    ]);
    let mut filesystem = routed(&world);

    let refused = filesystem.rename(path("/data/left/first"), path("/data/right/second"));

    assert!(matches!(
        refused,
        Err(Failure::FileSystem(FileSystemError::CrossMount))
    ));
    // Файловая система не получила вызов, и под пару не выдано ни одной capability.
    assert_eq!(world.minted(), 0);
    assert!(world.tree().contains_key("/data/left/first"));
    assert!(!world.tree().contains_key("/data/right/second"));
    assert!(world.holds("/data/left/first"));
}

#[test]
fn rename_inside_a_window_stays_on_its_root() {
    let world = World::new(&[("/window", Node::Directory), ("/window/first", file("one"))]);
    let mut filesystem = windowed(&world, "/window");

    filesystem
        .rename(path("/first"), path("/second"))
        .expect("rename");

    assert!(world.tree().contains_key("/window/second"));
    // Окно само определяет корень пары и не обращается к маршрутизатору.
    assert!(world.resolves().is_empty());
}

#[test]
fn rename_accepts_the_root_as_the_common_scope() {
    let world = World::new(&[("/first", file("one"))]);
    let mut filesystem = routed(&world);

    filesystem
        .rename(path("/first"), path("/second"))
        .expect("rename");

    assert!(world.holds("/second"));
    assert!(!world.holds("/first"));
}

#[test]
fn rename_refuses_a_root_length_outside_the_paths() {
    let world = World::new(&[("/data", Node::Directory), ("/data/first", file("one"))]);
    // Испорченный ответ маршрутизатора ставит длину корня за границей обоих путей.
    world.answers_pair_root_len(64);
    let mut filesystem = routed(&world);

    let refused = filesystem.rename(path("/data/first"), path("/data/second"));

    assert!(matches!(
        refused,
        Err(Failure::Transport(IpcError::InvalidValue))
    ));
    assert!(world.holds("/data/first"));
}

#[test]
fn rename_refuses_a_zero_root_length() {
    let world = World::new(&[("/data", Node::Directory), ("/data/first", file("one"))]);
    // Корень пары начинается с разделителя, поэтому пустой префикс путём не является.
    world.answers_pair_root_len(0);
    let mut filesystem = routed(&world);

    let refused = filesystem.rename(path("/data/first"), path("/data/second"));

    assert!(matches!(
        refused,
        Err(Failure::Transport(IpcError::InvalidValue))
    ));
    assert!(world.holds("/data/first"));
}

#[test]
fn rename_refuses_a_root_length_breaking_a_character() {
    let world = World::new(&[("/данные", Node::Directory), ("/данные/first", file("one"))]);
    // Длина попадает в середину двухбайтового символа имени.
    world.answers_pair_root_len(2);
    let mut filesystem = routed(&world);

    let refused = filesystem.rename(path("/данные/first"), path("/данные/second"));

    assert!(matches!(
        refused,
        Err(Failure::Transport(IpcError::InvalidValue))
    ));
    assert!(world.holds("/данные/first"));
}

#[test]
fn rename_refuses_equal_lengths_over_different_prefixes() {
    let world = World::mounted(&[(
        ROOT,
        &[
            ("/left", Node::Directory),
            ("/left/first", file("one")),
            ("/right", Node::Directory),
        ],
    )]);
    // Префиксы одной длины различаются, поэтому корень пары не накрывает обе стороны.
    world.answers_pair_root_len(5);
    let mut filesystem = routed(&world);

    let refused = filesystem.rename(path("/left/first"), path("/right/second"));

    assert!(matches!(
        refused,
        Err(Failure::Transport(IpcError::InvalidValue))
    ));
    assert!(world.holds("/left/first"));
    assert!(!world.holds("/right/second"));
}

#[test]
fn rename_refuses_a_root_length_off_the_component_boundary() {
    let world = World::new(&[("/data", Node::Directory), ("/data/first", file("one"))]);
    // Длина обрывает имя монтирования на середине компонента.
    world.answers_pair_root_len(3);
    let mut filesystem = routed(&world);

    let refused = filesystem.rename(path("/data/first"), path("/data/second"));

    assert!(matches!(
        refused,
        Err(Failure::Transport(IpcError::InvalidValue))
    ));
    assert!(world.holds("/data/first"));
}

#[test]
fn rename_refuses_a_pair_reply_that_does_not_decode() {
    let world = World::new(&[("/data", Node::Directory), ("/data/first", file("one"))]);
    world.truncates_the_pair_reply();
    let mut filesystem = routed(&world);

    let refused = filesystem.rename(path("/data/first"), path("/data/second"));

    // Сессия не принята из неразобранного кадра, и файловая система вызова не видела.
    assert!(matches!(
        refused,
        Err(Failure::Transport(IpcError::Truncated))
    ));
    assert_eq!(world.transports(), 1);
    assert!(world.holds("/data/first"));
    assert!(!world.holds("/data/second"));
}
