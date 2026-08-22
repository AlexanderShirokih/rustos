#![no_main]

//! Дифференциальный прогон изменяемого дерева: последовательность операций из входа исполняется
//! на `RamFileSystem` и на референсной модели дерева, а итоги сверяются пооперационно. В конце
//! прогона дерево сверяется с моделью целиком: пути, идентичность узлов, содержимое файлов,
//! перечисление каталогов и узлы, пережившие снятие последнего имени.

use std::{
    collections::{BTreeMap, BTreeSet},
    ops::{Deref, DerefMut},
};

use filesystem::{FileSystemBackend, HeapByteStore, NodeId, RamFileSystem as Backend};
use filesystem_abi::{
    DirCursor, DirEntry, ENTRY_HEADER_SIZE, FileSystemError, Listed, NAME_MAX, NodeKind, PATH_MAX,
    Path,
};
use libfuzzer_sys::fuzz_target;

const OPS_MAX: usize = 256;

const SHORT_NAMES: [&str; 5] = ["a", "b", "c", "dd", "e"];

const DEPTH_MAX: u8 = 3;

const SMALL_SIZE_MAX: u64 = 8 * 1024;

const OVERSIZE_MIN: u64 = 1 << 40;

const OVERSIZE: [u64; 3] = [OVERSIZE_MIN, u64::MAX - 1, u64::MAX];

const OVERSIZE_ODDS: u8 = 8;

const PAYLOAD_MAX: usize = 64;

const LIST_CAPACITY_MAX: usize = 4 * (ENTRY_HEADER_SIZE + NAME_MAX);

const LIST_BATCHES_MAX: usize = 2 * OPS_MAX;

struct RamFileSystem {
    backend: Backend,
    stores: HeapByteStore,
}

impl RamFileSystem {
    fn new() -> Self {
        Self {
            backend: Backend::new(),
            stores: HeapByteStore::new(),
        }
    }

    fn read_at(&self, node: NodeId, offset: u64, out: &mut [u8]) -> Result<usize, FileSystemError> {
        self.backend.read_at(&self.stores, node, offset, out)
    }

    fn write_at(
        &mut self,
        node: NodeId,
        offset: u64,
        data: &[u8],
    ) -> Result<usize, FileSystemError> {
        self.backend.write_at(&mut self.stores, node, offset, data)
    }

    fn append(&mut self, node: NodeId, data: &[u8]) -> Result<u64, FileSystemError> {
        self.backend.append(&mut self.stores, node, data)
    }

    fn set_size(&mut self, node: NodeId, size: u64) -> Result<(), FileSystemError> {
        self.backend.set_size(&mut self.stores, node, size)
    }
}

impl Deref for RamFileSystem {
    type Target = Backend;

    fn deref(&self) -> &Self::Target {
        &self.backend
    }
}

impl DerefMut for RamFileSystem {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.backend
    }
}

struct ModelNode {
    node: NodeId,
    kind: NodeKind,
    data: Vec<u8>,
}

struct Orphan {
    node: NodeId,
    kind: NodeKind,
    data: Vec<u8>,
}

struct Model {
    nodes: BTreeMap<String, ModelNode>,
    issued: BTreeSet<NodeId>,
    orphans: Vec<Orphan>,
}

struct Input<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl Model {
    fn new() -> Self {
        let root = ModelNode {
            node: NodeId::from_raw(0),
            kind: NodeKind::Directory,
            data: Vec::new(),
        };
        Self {
            nodes: BTreeMap::from([(String::from("/"), root)]),
            issued: BTreeSet::from([NodeId::from_raw(0)]),
            orphans: Vec::new(),
        }
    }

    fn resolve(&self, path: &str) -> Result<&ModelNode, FileSystemError> {
        let mut current = String::from("/");
        let mut node = self.nodes.get(&current).expect("root node");
        for component in components(path) {
            if node.kind == NodeKind::File {
                return Err(FileSystemError::NotADirectory);
            }
            current = join(&current, component);
            node = self.nodes.get(&current).ok_or(FileSystemError::NotFound)?;
        }
        Ok(node)
    }

    fn directory_of(&self, path: &str) -> Result<&ModelNode, FileSystemError> {
        let (parent, _) = split_last(path).ok_or(FileSystemError::InvalidPath)?;
        let node = self.resolve(parent)?;
        if node.kind == NodeKind::File {
            return Err(FileSystemError::NotADirectory);
        }
        Ok(node)
    }

    fn children(&self, path: &str) -> Vec<(String, NodeKind)> {
        self.nodes
            .iter()
            .filter_map(|(key, node)| {
                let (parent, name) = split_last(key)?;
                (parent == path).then(|| (String::from(name), node.kind))
            })
            .collect()
    }

    fn size_of(&self, path: &str, node: &ModelNode) -> u64 {
        match node.kind {
            NodeKind::File => node.data.len() as u64,
            NodeKind::Directory => self.children(path).len() as u64,
        }
    }

    fn insert(&mut self, path: &str, node: NodeId, kind: NodeKind) {
        assert!(
            self.issued.insert(node),
            "node {node:?} handed out for {path} a second time"
        );
        self.nodes.insert(
            String::from(path),
            ModelNode {
                node,
                kind,
                data: Vec::new(),
            },
        );
    }

    fn detach(&mut self, path: &str) {
        let Some(node) = self.nodes.remove(path) else {
            return;
        };
        self.orphans.push(Orphan {
            node: node.node,
            kind: node.kind,
            data: node.data,
        });
    }

    fn file_mut(&mut self, path: &str) -> &mut Vec<u8> {
        &mut self.nodes.get_mut(path).expect("node under the path").data
    }
}

impl<'a> Input<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, at: 0 }
    }

    fn is_empty(&self) -> bool {
        self.at >= self.bytes.len()
    }

    fn byte(&mut self) -> u8 {
        let byte = self.bytes.get(self.at).copied().unwrap_or(0);
        self.at += 1;
        byte
    }

    fn path(&mut self, long_name: &str) -> String {
        let depth = self.byte() % (DEPTH_MAX + 1);
        let mut path = String::from("/");
        for index in 0..depth {
            if index > 0 {
                path.push('/');
            }
            let name = self.name(long_name);
            path.push_str(name);
        }
        path
    }

    fn name<'n>(&mut self, long_name: &'n str) -> &'n str {
        let choice = self.byte() as usize % (SHORT_NAMES.len() + 1);
        SHORT_NAMES.get(choice).copied().unwrap_or(long_name)
    }

    fn payload(&mut self) -> Vec<u8> {
        let len = self.byte() as usize % (PAYLOAD_MAX + 1);
        (0..len).map(|_| self.byte()).collect()
    }

    fn size(&mut self) -> u64 {
        let choice = self.byte();
        if let Some(oversize) = OVERSIZE.get((choice % OVERSIZE_ODDS) as usize) {
            return *oversize;
        }
        u64::from(u16::from_le_bytes([choice, self.byte()])) % (SMALL_SIZE_MAX + 1)
    }
}

fuzz_target!(|data: &[u8]| {
    let long_name = "l".repeat(NAME_MAX);
    let mut fs = RamFileSystem::new();
    let mut model = Model::new();
    let mut input = Input::new(data);

    for _ in 0..OPS_MAX {
        if input.is_empty() {
            break;
        }
        let operation = input.byte();
        let path = input.path(&long_name);
        if Path::new(&path).is_err() {
            continue;
        }
        match operation % 9 {
            0 => lookup(&fs, &model, &path),
            1 => {
                let kind = kind(input.byte());
                create(&mut fs, &mut model, &path, kind);
            }
            2 => unlink(&mut fs, &mut model, &path),
            3 => {
                let to = input.path(&long_name);
                rename(&mut fs, &mut model, &path, &to);
            }
            4 => {
                let offset = input.size();
                let payload = input.payload();
                write_at(&mut fs, &mut model, &path, offset, &payload);
            }
            5 => {
                let payload = input.payload();
                append(&mut fs, &mut model, &path, &payload);
            }
            6 => {
                let size = input.size();
                set_size(&mut fs, &mut model, &path, size);
            }
            7 => {
                let offset = input.size();
                let len = input.byte() as usize;
                read_at(&fs, &model, &path, offset, len);
            }
            _ => {
                let capacity = input.byte() as usize % LIST_CAPACITY_MAX;
                list(&fs, &model, &path, capacity);
            }
        }
    }

    verify(&fs, &model);
});

fn kind(byte: u8) -> NodeKind {
    if byte.is_multiple_of(2) {
        NodeKind::File
    } else {
        NodeKind::Directory
    }
}

fn path_of(text: &str) -> Path<'_> {
    Path::new(text).expect("path checked before the operation")
}

fn components(path: &str) -> impl Iterator<Item = &str> {
    path[1..].split('/').filter(|name| !name.is_empty())
}

fn join(parent: &str, name: &str) -> String {
    if parent == "/" {
        return String::from("/") + name;
    }
    String::from(parent) + "/" + name
}

fn split_last(path: &str) -> Option<(&str, &str)> {
    if path == "/" {
        return None;
    }
    let cut = path.rfind('/')?;
    let parent = if cut == 0 { "/" } else { &path[..cut] };
    Some((parent, &path[cut + 1..]))
}

fn node_of(fs: &RamFileSystem, model: &Model, path: &str) -> Option<NodeId> {
    let expected = model.resolve(path).map(|node| node.node);
    let found = fs.lookup(path_of(path));
    assert_eq!(found, expected, "lookup {path}");
    found.ok()
}

fn lookup(fs: &RamFileSystem, model: &Model, path: &str) {
    node_of(fs, model, path);
}

fn create(fs: &mut RamFileSystem, model: &mut Model, path: &str, kind: NodeKind) {
    let expected = expected_create(model, path);
    let created = fs.create(path_of(path), kind);
    if let Err(error) = expected {
        assert_eq!(created, Err(error), "create {path}");
    } else {
        let node = created.unwrap_or_else(|error| panic!("create {path}: {error:?}"));
        model.insert(path, node, kind);
    }
}

fn expected_create(model: &Model, path: &str) -> Result<(), FileSystemError> {
    model.directory_of(path)?;
    if model.nodes.contains_key(path) {
        return Err(FileSystemError::AlreadyExists);
    }
    Ok(())
}

fn unlink(fs: &mut RamFileSystem, model: &mut Model, path: &str) {
    let expected = expected_unlink(model, path);
    assert_eq!(fs.unlink(path_of(path)), expected, "unlink {path}");
    if expected.is_ok() {
        model.detach(path);
    }
}

fn expected_unlink(model: &Model, path: &str) -> Result<(), FileSystemError> {
    model.directory_of(path)?;
    let node = model.nodes.get(path).ok_or(FileSystemError::NotFound)?;
    if node.kind == NodeKind::Directory && !model.children(path).is_empty() {
        return Err(FileSystemError::NotEmpty);
    }
    Ok(())
}

fn rename(fs: &mut RamFileSystem, model: &mut Model, from: &str, to: &str) {
    if Path::new(to).is_err() {
        return;
    }
    let expected = expected_rename(model, from, to);
    assert_eq!(
        fs.rename(path_of(from), path_of(to)),
        expected,
        "rename {from} -> {to}"
    );
    if expected.is_ok() && from != to {
        // Замещённое имя теряет свой узел: он остаётся в дереве без единого имени.
        model.detach(to);
        move_subtree(model, from, to);
    }
}

fn expected_rename(model: &Model, from: &str, to: &str) -> Result<(), FileSystemError> {
    model.directory_of(from)?;
    let node = model.nodes.get(from).ok_or(FileSystemError::NotFound)?;
    if from == to {
        return Ok(());
    }
    // Перенос каталога внутрь себя рвёт дерево: путь назначения лежит под источником.
    if node.kind == NodeKind::Directory && to.starts_with(&(String::from(from) + "/")) {
        return Err(FileSystemError::InvalidPath);
    }
    model.directory_of(to)?;
    if model
        .nodes
        .get(to)
        .is_some_and(|occupant| occupant.kind == NodeKind::Directory)
    {
        return Err(FileSystemError::IsADirectory);
    }
    Ok(())
}

fn move_subtree(model: &mut Model, from: &str, to: &str) {
    let prefix = String::from(from) + "/";
    let moved: Vec<String> = model
        .nodes
        .keys()
        .filter(|key| key.as_str() == from || key.starts_with(&prefix))
        .cloned()
        .collect();
    for key in moved {
        let node = model.nodes.remove(&key).expect("key taken from the model");
        let target = String::from(to) + &key[from.len()..];
        model.nodes.insert(target, node);
    }
}

fn write_at(fs: &mut RamFileSystem, model: &mut Model, path: &str, offset: u64, data: &[u8]) {
    let Some(node) = node_of(fs, model, path) else {
        return;
    };
    let expected = expected_write(model, path, offset);
    let written = fs.write_at(node, offset, data);
    if let Err(error) = expected {
        assert_eq!(written, Err(error), "write {path} at {offset}");
    } else {
        assert_eq!(written, Ok(data.len()), "write {path} at {offset}");
        write_into(model.file_mut(path), offset as usize, data);
    }
}

fn expected_write(model: &Model, path: &str, offset: u64) -> Result<(), FileSystemError> {
    let node = model.resolve(path)?;
    if node.kind == NodeKind::Directory {
        return Err(FileSystemError::IsADirectory);
    }
    // Смещение генерируется либо внутри потолка файла, либо за OVERSIZE_MIN: промежуток между
    // ними прогон не выдаёт, поэтому одной границы хватает.
    if offset >= OVERSIZE_MIN {
        return Err(FileSystemError::NoSpace);
    }
    Ok(())
}

fn append(fs: &mut RamFileSystem, model: &mut Model, path: &str, data: &[u8]) {
    let Some(node) = node_of(fs, model, path) else {
        return;
    };
    let expected = expected_append(model, path);
    let appended = fs.append(node, data);
    match expected {
        Err(error) => assert_eq!(appended, Err(error), "append {path}"),
        Ok(offset) => {
            assert_eq!(appended, Ok(offset), "append {path}");
            write_into(model.file_mut(path), offset as usize, data);
        }
    }
}

fn expected_append(model: &Model, path: &str) -> Result<u64, FileSystemError> {
    let node = model.resolve(path)?;
    if node.kind == NodeKind::Directory {
        return Err(FileSystemError::IsADirectory);
    }
    let offset = node.data.len() as u64;
    expected_write(model, path, offset)?;
    Ok(offset)
}

fn set_size(fs: &mut RamFileSystem, model: &mut Model, path: &str, size: u64) {
    let Some(node) = node_of(fs, model, path) else {
        return;
    };
    let expected = expected_set_size(model, path, size);
    assert_eq!(
        fs.set_size(node, size),
        expected,
        "set_size {path} to {size}"
    );
    if expected.is_ok() {
        model.file_mut(path).resize(size as usize, 0);
    }
}

fn expected_set_size(model: &Model, path: &str, size: u64) -> Result<(), FileSystemError> {
    let node = model.resolve(path)?;
    if node.kind == NodeKind::Directory {
        return Err(FileSystemError::IsADirectory);
    }
    if size >= OVERSIZE_MIN {
        return Err(FileSystemError::NoSpace);
    }
    Ok(())
}

fn read_at(fs: &RamFileSystem, model: &Model, path: &str, offset: u64, len: usize) {
    let Some(node) = node_of(fs, model, path) else {
        return;
    };
    let mut out = vec![0; len];
    let read = fs.read_at(node, offset, &mut out);
    let model_node = model.resolve(path).expect("node under the path");
    if model_node.kind == NodeKind::Directory {
        assert_eq!(
            read,
            Err(FileSystemError::IsADirectory),
            "read {path} at {offset}"
        );
        return;
    }
    let start = usize::try_from(offset).unwrap_or(usize::MAX);
    let tail = model_node.data.get(start..).unwrap_or_default();
    let count = tail.len().min(len);
    assert_eq!(read, Ok(count), "read {path} at {offset}");
    assert_eq!(&out[..count], &tail[..count], "bytes of {path} at {offset}");
}

fn list(fs: &RamFileSystem, model: &Model, path: &str, capacity: usize) {
    let Some(node) = node_of(fs, model, path) else {
        return;
    };
    if model.resolve(path).expect("node under the path").kind == NodeKind::File {
        assert_eq!(
            fs.list(node, DirCursor::START, &mut vec![0; capacity]),
            Err(FileSystemError::NotADirectory),
            "list {path}"
        );
        return;
    }
    let Some(listed) = walk(fs, node, capacity) else {
        return;
    };
    assert_entries(&listed, &model.children(path), path);
}

fn walk(fs: &RamFileSystem, node: NodeId, capacity: usize) -> Option<Vec<(String, NodeKind)>> {
    let mut out = vec![0; capacity];
    let mut names = Vec::new();
    let mut from = DirCursor::START;
    for _ in 0..LIST_BATCHES_MAX {
        let listed = fs
            .list(node, from, &mut out)
            .unwrap_or_else(|error| panic!("list of a directory: {error:?}"));
        collect(&out, listed, &mut names);
        if listed.end {
            return Some(names);
        }
        // Незавершённый обход с непустой пачкой обязан продвинуться: позиция продолжения строго
        // больше входной.
        assert!(
            listed.count == 0 || listed.next.raw() > from.raw(),
            "listing stalled at slot {}",
            from.raw()
        );
        if listed.count == 0 {
            return None;
        }
        from = listed.next;
    }
    panic!("listing did not converge in {LIST_BATCHES_MAX} batches");
}

fn collect(out: &[u8], listed: Listed, names: &mut Vec<(String, NodeKind)>) {
    let mut offset = 0;
    for _ in 0..listed.count {
        let decoded = DirEntry::decode(&out[offset..]).expect("record written by list");
        names.push((String::from(decoded.entry.name), decoded.entry.kind));
        offset += decoded.len;
    }
}

fn assert_entries(listed: &[(String, NodeKind)], expected: &[(String, NodeKind)], path: &str) {
    let mut seen = listed.to_vec();
    seen.sort_by(|left, right| left.0.cmp(&right.0));
    let mut want = expected.to_vec();
    want.sort_by(|left, right| left.0.cmp(&right.0));
    assert_eq!(seen, want, "listing of {path}");
}

fn verify(fs: &RamFileSystem, model: &Model) {
    for (path, node) in &model.nodes {
        // Перенос каталога уводит его записи на глубину, чей путь целиком выходит за границу
        // контракта: до таких узлов ведёт имя в каталоге, а путь от корня к ним невыразим.
        if path.len() <= PATH_MAX {
            assert_eq!(fs.lookup(path_of(path)), Ok(node.node), "lookup {path}");
        }
        let info = fs
            .metadata(node.node)
            .unwrap_or_else(|error| panic!("metadata of {path}: {error:?}"));
        assert_eq!(info.kind, node.kind, "kind of {path}");
        assert_eq!(info.size, model.size_of(path, node), "size of {path}");
        match node.kind {
            NodeKind::File => assert_bytes(fs, node.node, &node.data, path),
            NodeKind::Directory => {
                let listed = walk(fs, node.node, LIST_CAPACITY_MAX).expect("room for a record");
                assert_entries(&listed, &model.children(path), path);
            }
        }
    }
    // Узел переживает снятие последнего имени: за ним закреплена идентичность уже открытых
    // дескрипторов, и содержимое его больше не меняется.
    for orphan in &model.orphans {
        let info = fs
            .metadata(orphan.node)
            .unwrap_or_else(|error| panic!("metadata of an unnamed node: {error:?}"));
        assert_eq!(info.kind, orphan.kind, "kind of an unnamed node");
        if orphan.kind == NodeKind::File {
            assert_eq!(
                info.size,
                orphan.data.len() as u64,
                "size of an unnamed node"
            );
            assert_bytes(fs, orphan.node, &orphan.data, "an unnamed node");
        }
    }
}

fn assert_bytes(fs: &RamFileSystem, node: NodeId, expected: &[u8], path: &str) {
    let mut out = vec![0; expected.len()];
    let count = fs
        .read_at(node, 0, &mut out)
        .unwrap_or_else(|error| panic!("read of {path}: {error:?}"));
    assert_eq!(count, expected.len(), "byte count of {path}");
    assert_eq!(out, expected, "bytes of {path}");
}

fn write_into(file: &mut Vec<u8>, start: usize, data: &[u8]) {
    let end = start + data.len();
    if file.len() < end {
        file.resize(end, 0);
    }
    file[start..end].copy_from_slice(data);
}
