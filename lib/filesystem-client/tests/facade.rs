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
    NodeDescriptor, NodeInfo, NodeKind, OpenFlags, Opened, PATH_MAX, Written, file_system,
    namespace,
};
use filesystem_client::{Environment, Failure, FileSystem, SharedRegion};
use ipc::{
    Timeout, Transport,
    wire::{Cap, Str},
};
use ipc_test::MockEnd;

const BUFFER_SIZE: u32 = 4096;

const ROOT: &str = "/";

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
    sessions: Mutex<BTreeMap<u32, MockEnd>>,
    buffers: Mutex<BTreeMap<u32, Shared>>,
    resolved: Mutex<Vec<String>>,
    protected: Mutex<Option<String>>,
    next_cap: AtomicU32,
    reads: AtomicUsize,
    live: AtomicUsize,
    poison: AtomicBool,
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
            protected: Mutex::new(None),
            next_cap: AtomicU32::new(1),
            reads: AtomicUsize::new(0),
            live: AtomicUsize::new(0),
            poison: AtomicBool::new(false),
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
        // Выбор монтирования повторяет маршрутизатор, беря самый длинный совпавший префикс.
        let mount = self
            .mounts
            .iter()
            .filter(|mount| under(mount.prefix.as_str(), path))
            .max_by_key(|mount| mount.prefix.len())?;
        let tail = if mount.prefix == ROOT {
            String::from(path)
        } else {
            path.strip_prefix(mount.prefix.as_str())
                .filter(|rest| !rest.is_empty())
                .map_or_else(|| String::from(ROOT), String::from)
        };
        Some((&mount.filesystem, tail))
    }

    fn cap(&self) -> Cap {
        let raw = self.next_cap.fetch_add(1, Ordering::SeqCst);
        Cap::from_raw(core::num::NonZeroU32::new(raw).expect("capability id is non-zero"))
    }

    fn resolves(&self) -> Vec<String> {
        self.resolved.lock().expect("mutex not poisoned").clone()
    }

    fn holds(&self, path: &str) -> bool {
        self.covering(path)
            .is_some_and(|(filesystem, tail)| filesystem.tree().contains_key(&tail))
    }

    fn session_end(self: &Arc<Self>, path: &str) -> MockEnd {
        let (filesystem, root) = self.covering(path).expect("a mount covers the path");
        self.session_end_in(Arc::clone(filesystem), root)
    }

    fn session_end_in(self: &Arc<Self>, filesystem: Arc<Filesystem>, root: String) -> MockEnd {
        let (client, server) = MockEnd::pair();
        let world = Arc::clone(self);
        thread::spawn(move || serve_session(&world, filesystem, root, &server));
        client
    }

    fn session(self: &Arc<Self>, filesystem: Arc<Filesystem>, root: String) -> Cap {
        let cap = self.cap();
        let end = self.session_end_in(filesystem, root);
        self.sessions
            .lock()
            .expect("mutex not poisoned")
            .insert(cap.raw().get(), end);
        cap
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

    fn data(&self) -> Result<MutexGuard<'_, Vec<u8>>, FileSystemError> {
        Ok(self
            .buffer
            .as_ref()
            .ok_or(FileSystemError::NoBuffer)?
            .lock()
            .expect("mutex not poisoned"))
    }
}

impl file_system::Service for SessionServer {
    fn acquire_buffer(&mut self, size: u32) -> Result<Cap, FileSystemError> {
        let shared: Shared = Arc::new(Mutex::new(std::vec![0; size as usize]));
        let cap = self.world.cap();
        self.world
            .buffers
            .lock()
            .expect("mutex not poisoned")
            .insert(cap.raw().get(), Arc::clone(&shared));
        self.buffer = Some(shared);
        Ok(cap)
    }

    fn open(
        &mut self,
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

    fn close(&mut self, fd: NodeDescriptor) -> Result<u32, FileSystemError> {
        self.open
            .remove(&fd.raw())
            .ok_or(FileSystemError::BadDescriptor)?;
        self.world.live.fetch_sub(1, Ordering::SeqCst);
        Ok(0)
    }

    fn read(&mut self, fd: NodeDescriptor, offset: u64, len: u32) -> Result<u32, FileSystemError> {
        self.world.reads.fetch_add(1, Ordering::SeqCst);
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
        fd: NodeDescriptor,
        offset: u64,
        len: u32,
    ) -> Result<Written, FileSystemError> {
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

    fn set_size(&mut self, _fd: NodeDescriptor, _size: u64) -> Result<u32, FileSystemError> {
        Err(FileSystemError::NotSupported)
    }

    fn stat(&mut self, fd: NodeDescriptor) -> Result<NodeInfo, FileSystemError> {
        let path = self.path_of(fd)?;
        let tree = self.filesystem.tree();
        let node = tree.get(&path).ok_or(FileSystemError::NotFound)?.clone();
        Ok(NodeInfo {
            kind: kind_of(&node),
            size: measure(&tree, &path, &node),
        })
    }

    fn list(&mut self, fd: NodeDescriptor, from: DirCursor) -> Result<Listed, FileSystemError> {
        let path = self.path_of(fd)?;
        let tree = self.filesystem.tree();
        if tree.get(&path) != Some(&Node::Directory) {
            return Err(FileSystemError::NotADirectory);
        }
        let children = children_of(&tree, &path);
        drop(tree);
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

    fn unlink(&mut self, path: Str<'_, { PATH_MAX }>) -> Result<u32, FileSystemError> {
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
        if node == Node::Directory && !children_of(&tree, &full).is_empty() {
            return Err(FileSystemError::NotEmpty);
        }
        tree.remove(&full);
        Ok(0)
    }

    fn create_backed(
        &mut self,
        _path: Str<'_, { PATH_MAX }>,
        _region: Cap,
        _size: u64,
    ) -> Result<u32, FileSystemError> {
        Err(FileSystemError::NotSupported)
    }

    fn backing_memory(&mut self, _fd: NodeDescriptor) -> Result<Cap, FileSystemError> {
        Err(FileSystemError::NotSupported)
    }

    fn open_root(&mut self, _path: Str<'_, { PATH_MAX }>) -> Result<Cap, FileSystemError> {
        Err(FileSystemError::NotSupported)
    }
}

struct Router {
    world: Arc<World>,
}

impl namespace::Service for Router {
    fn resolve(&mut self, path: Str<'_, { PATH_MAX }>) -> Result<Cap, FileSystemError> {
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

struct TestEnvironment {
    world: Arc<World>,
}

impl Environment for TestEnvironment {
    type Transport = MockEnd;
    type Region = TestBuffer;

    fn connect(&self, port: Cap) -> Result<MockEnd, FileSystemError> {
        self.world
            .sessions
            .lock()
            .expect("mutex not poisoned")
            .remove(&port.raw().get())
            .ok_or(FileSystemError::NoSession)
    }

    fn map(&self, region: Cap) -> Result<TestBuffer, FileSystemError> {
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

fn serve_session(world: &Arc<World>, filesystem: Arc<Filesystem>, root: String, end: &MockEnd) {
    let mut server = SessionServer {
        world: Arc::clone(world),
        filesystem,
        root,
        buffer: None,
        open: BTreeMap::new(),
        next_descriptor: 1,
    };
    while end.wait_readable(Timeout::INFINITE).is_ok() {
        if world.poison.swap(false, Ordering::SeqCst) {
            return;
        }
        if file_system::dispatch(&mut server, end).is_err() {
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
    let (client, server) = MockEnd::pair();
    let mut router = Router {
        world: Arc::clone(world),
    };
    thread::spawn(move || {
        while server.wait_readable(Timeout::INFINITE).is_ok() {
            if namespace::dispatch(&mut router, &server).is_err() {
                return;
            }
        }
    });
    FileSystem::routed(
        TestEnvironment {
            world: Arc::clone(world),
        },
        client,
    )
}

fn windowed(world: &Arc<World>, root: &str) -> FileSystem<TestEnvironment> {
    FileSystem::windowed(
        TestEnvironment {
            world: Arc::clone(world),
        },
        world.session_end(root),
    )
}

fn read_all(filesystem: &mut FileSystem<TestEnvironment>, path: &str, len: usize) -> Vec<u8> {
    let mut node = filesystem
        .open(path, AccessMode::Read, OpenFlags::NONE)
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

    let outcome = filesystem.open("/data/absent", AccessMode::Read, OpenFlags::NONE);

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

    let outcome = filesystem.open("/data/first", AccessMode::Read, OpenFlags::NONE);

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
        .open("/data/first", AccessMode::Read, OpenFlags::NONE)
        .expect("open");
    assert_eq!(world.live.load(Ordering::SeqCst), 1);
    drop(node);

    assert_eq!(world.live.load(Ordering::SeqCst), 0);
}

#[test]
fn sequential_reads_continue_from_the_position() {
    let world = World::new(&[("/data", Node::Directory), ("/data/first", file("abcdef"))]);
    let mut filesystem = routed(&world);
    let mut node = filesystem
        .open("/data/first", AccessMode::Read, OpenFlags::NONE)
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
        .open("/data/first", AccessMode::ReadWrite, OpenFlags::NONE)
        .expect("open");
    assert_eq!(node.write(body.as_bytes()).expect("write"), body.len());
    assert_eq!(node.position(), body.len() as u64);
    drop(node);

    assert_eq!(
        read_all(&mut filesystem, "/data/first", body.len()),
        body.as_bytes()
    );
}

#[test]
fn append_lands_at_the_end_of_the_node() {
    let world = World::new(&[("/data", Node::Directory), ("/data/first", file("one"))]);
    let mut filesystem = routed(&world);
    let node = filesystem
        .open("/data/first", AccessMode::Write, OpenFlags::NONE)
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
fn append_beyond_the_buffer_is_refused() {
    let body = "e".repeat(BUFFER_SIZE as usize + 1);
    let world = World::new(&[("/data", Node::Directory), ("/data/first", file(""))]);
    let mut filesystem = routed(&world);
    let node = filesystem
        .open("/data/first", AccessMode::Write, OpenFlags::NONE)
        .expect("open");

    assert!(matches!(
        node.append(body.as_bytes()).err(),
        Some(Failure::FileSystem(FileSystemError::BufferTooSmall))
    ));
}

#[test]
fn metadata_reports_the_kind_and_the_size() {
    let world = World::new(&[("/data", Node::Directory), ("/data/first", file("one"))]);
    let mut filesystem = routed(&world);

    assert_eq!(
        filesystem.metadata("/data/first").expect("metadata"),
        NodeInfo {
            kind: NodeKind::File,
            size: 3,
        }
    );
    assert_eq!(
        filesystem.metadata("/data").expect("metadata"),
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

    let mut listing = filesystem.read_directory("/data").expect("listing");
    let mut names = Vec::new();
    while let Some(entry) = listing.next_entry().expect("entry") {
        names.push(String::from(entry.name));
    }

    assert_eq!(names.len(), ENTRIES);
    assert!(names.contains(&String::from("entry000")));
    assert!(names.contains(&String::from("entry059")));
}

#[test]
fn empty_directory_yields_no_entries() {
    let world = World::new(&[("/data", Node::Directory)]);
    let mut filesystem = routed(&world);

    let mut listing = filesystem.read_directory("/data").expect("listing");

    assert!(listing.next_entry().expect("entry").is_none());
}

#[test]
fn nodes_are_created_removed_and_renamed() {
    let world = World::new(&[("/data", Node::Directory)]);
    let mut filesystem = routed(&world);

    let node = filesystem
        .open("/data/first", AccessMode::ReadWrite, OpenFlags::CREATE)
        .expect("create");
    drop(node);
    filesystem.create_directory("/data/nested").expect("mkdir");
    assert!(world.holds("/data/first"));
    assert!(world.holds("/data/nested"));

    filesystem
        .rename("/data/first", "/data/second")
        .expect("rename");
    assert!(!world.holds("/data/first"));
    assert!(world.holds("/data/second"));

    filesystem.remove("/data/second").expect("remove");
    filesystem.remove("/data/nested").expect("remove");
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

    filesystem.remove_tree("/data/tree").expect("remove tree");

    assert!(!world.holds("/data/tree"));
    assert!(!world.holds("/data/tree/nested/second"));
    assert!(world.holds("/data/keep"));
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

    let outcome = filesystem.remove_tree("/data/tree");

    assert!(matches!(
        outcome.err(),
        Some(Failure::FileSystem(FileSystemError::ReadOnly))
    ));
    assert!(world.holds("/data/tree/nested/second"));
    assert!(world.holds("/data/tree"));
}

#[test]
fn a_path_beyond_the_bound_never_reaches_the_router() {
    let world = World::new(&[("/data", Node::Directory)]);
    let mut filesystem = routed(&world);
    let long = std::format!("/{}", "p".repeat(PATH_MAX));

    let outcome = filesystem.open(&long, AccessMode::Read, OpenFlags::NONE);

    assert!(matches!(
        outcome.err(),
        Some(Failure::FileSystem(FileSystemError::InvalidPath))
    ));
    assert!(world.resolves().is_empty());
}

#[test]
fn a_name_beyond_the_bound_never_reaches_the_router() {
    let world = World::new(&[("/data", Node::Directory)]);
    let mut filesystem = routed(&world);
    let long = std::format!("/data/{}", "p".repeat(NAME_MAX + 1));

    let outcome = filesystem.open(&long, AccessMode::Read, OpenFlags::NONE);

    assert!(matches!(
        outcome.err(),
        Some(Failure::FileSystem(FileSystemError::InvalidPath))
    ));
    assert!(world.resolves().is_empty());
}

#[test]
fn a_relative_path_is_refused() {
    let world = World::new(&[("/data", Node::Directory)]);
    let mut filesystem = routed(&world);

    let outcome = filesystem.open("data/first", AccessMode::Read, OpenFlags::NONE);

    assert!(matches!(
        outcome.err(),
        Some(Failure::FileSystem(FileSystemError::InvalidPath))
    ));
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

    let mut listing = filesystem.read_directory("/tmp").expect("listing");
    let mut names = Vec::new();
    while let Some(entry) = listing.next_entry().expect("entry") {
        names.push(String::from(entry.name));
    }
    drop(listing);

    assert_eq!(names, [String::from("first"), String::from("nested")]);
    assert_eq!(
        filesystem.metadata("/tmp").expect("metadata"),
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
        filesystem.metadata("/store/area").expect("metadata"),
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

    let outcome = filesystem.remove("/tmp");

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

    let outcome = filesystem.remove_tree("/tmp");

    assert!(matches!(
        outcome.err(),
        Some(Failure::FileSystem(FileSystemError::InvalidPath))
    ));
    assert!(world.holds("/tmp/first"));

    let mut window = windowed(&world, "/tmp");
    assert!(matches!(
        window.remove_tree(ROOT).err(),
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
        .rename("/data/left/first", "/data/right/second")
        .expect("rename");

    assert!(!world.holds("/data/left/first"));
    assert!(world.holds("/data/right/second"));
    assert_eq!(world.resolves(), [String::from("/data")]);
}
