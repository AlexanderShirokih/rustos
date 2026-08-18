//! Изменяемое дерево в памяти сервера: узлы в арене, файл - байтовый вектор,
//! каталог - список слотов с надгробиями на месте удалённых записей. Надгробие
//! переиспользуется под новое имя каталога.

use alloc::{string::String, vec, vec::Vec};

use filesystem_abi::{DirCursor, DirEntry, FileSystemError, Listed, NodeInfo, NodeKind, Path};
use userland::USERLAND_PAGE_SIZE;

use crate::{
    backend::FileSystemBackend,
    node::{NodeId, Window},
    store::{ByteStore, StoreId},
};

const ROOT: NodeId = NodeId::from_raw(0);

const FILE_SIZE_MAX: u64 = 16 * 1024 * 1024;

const NODE_LIMIT: usize = 4096;

struct Slot {
    name: String,
    node: NodeId,
}

enum Node {
    File(File),
    Directory(Vec<Option<Slot>>),
}

enum File {
    Heap(Vec<u8>),
    Backed {
        store: StoreId,
        len: usize,
        capacity: usize,
        pinned: bool,
    },
}

/// Дерево в куче сервера. Узел переживает снятие последнего имени: за ним
/// закреплена идентичность уже открытых дескрипторов.
pub struct RamFileSystem {
    nodes: Vec<Node>,
}

impl RamFileSystem {
    /// Дерево из одного пустого корневого каталога.
    pub fn new() -> Self {
        Self {
            nodes: vec![Node::Directory(Vec::new())],
        }
    }

    fn node(&self, id: NodeId) -> Result<&Node, FileSystemError> {
        self.nodes
            .get(id.raw() as usize)
            .ok_or(FileSystemError::NotFound)
    }

    fn entries(&self, directory: NodeId) -> Result<&[Option<Slot>], FileSystemError> {
        match self.node(directory)? {
            Node::Directory(entries) => Ok(entries),
            Node::File(_) => Err(FileSystemError::NotADirectory),
        }
    }

    fn entries_mut(
        &mut self,
        directory: NodeId,
    ) -> Result<&mut Vec<Option<Slot>>, FileSystemError> {
        let index = directory.raw() as usize;
        match self.nodes.get_mut(index).ok_or(FileSystemError::NotFound)? {
            Node::Directory(entries) => Ok(entries),
            Node::File(_) => Err(FileSystemError::NotADirectory),
        }
    }

    fn file_node(&self, node: NodeId) -> Result<&File, FileSystemError> {
        match self.node(node)? {
            Node::File(file) => Ok(file),
            Node::Directory(_) => Err(FileSystemError::IsADirectory),
        }
    }

    fn file_node_mut(&mut self, node: NodeId) -> Result<&mut File, FileSystemError> {
        let index = node.raw() as usize;
        match self.nodes.get_mut(index).ok_or(FileSystemError::NotFound)? {
            Node::File(file) => Ok(file),
            Node::Directory(_) => Err(FileSystemError::IsADirectory),
        }
    }

    fn locate(
        &self,
        directory: NodeId,
        name: &str,
    ) -> Result<Option<(usize, NodeId)>, FileSystemError> {
        Ok(self
            .entries(directory)?
            .iter()
            .enumerate()
            .find_map(|(index, slot)| {
                slot.as_ref()
                    .filter(|entry| entry.name == name)
                    .map(|entry| (index, entry.node))
            }))
    }

    fn parent_of<'a>(&self, path: Path<'a>) -> Result<(NodeId, &'a str), FileSystemError> {
        let (parent, name) = path.split_last().ok_or(FileSystemError::InvalidPath)?;
        Ok((self.lookup(parent)?, name.as_str()))
    }

    fn insert(&mut self, node: Node) -> Result<NodeId, FileSystemError> {
        if self.nodes.len() >= NODE_LIMIT {
            return Err(FileSystemError::NoSpace);
        }
        self.nodes
            .try_reserve(1)
            .map_err(|_| FileSystemError::NoSpace)?;
        let id = NodeId::from_raw(self.nodes.len() as u32);
        self.nodes.push(node);
        Ok(id)
    }

    fn attach(
        &mut self,
        directory: NodeId,
        name: &str,
        node: NodeId,
    ) -> Result<(), FileSystemError> {
        let mut owned = String::new();
        owned
            .try_reserve_exact(name.len())
            .map_err(|_| FileSystemError::NoSpace)?;
        owned.push_str(name);
        let slot = Some(Slot { name: owned, node });

        if let Some((index, _)) = self.locate(directory, name)? {
            self.entries_mut(directory)?[index] = slot;
            return Ok(());
        }
        let entries = self.entries_mut(directory)?;
        if let Some(vacant) = entries.iter().position(Option::is_none) {
            entries[vacant] = slot;
            return Ok(());
        }
        entries
            .try_reserve(1)
            .map_err(|_| FileSystemError::NoSpace)?;
        entries.push(slot);
        Ok(())
    }
}

impl Default for RamFileSystem {
    fn default() -> Self {
        Self::new()
    }
}

impl FileSystemBackend for RamFileSystem {
    fn is_mutable(&self) -> bool {
        true
    }

    fn lookup(&self, path: Path<'_>) -> Result<NodeId, FileSystemError> {
        let mut current = ROOT;
        for component in path.components() {
            let (_, node) = self
                .locate(current, component.as_str())?
                .ok_or(FileSystemError::NotFound)?;
            current = node;
        }
        Ok(current)
    }

    fn create(&mut self, path: Path<'_>, kind: NodeKind) -> Result<NodeId, FileSystemError> {
        let (directory, name) = self.parent_of(path)?;
        if self.locate(directory, name)?.is_some() {
            return Err(FileSystemError::AlreadyExists);
        }
        let node = self.insert(match kind {
            NodeKind::File => Node::File(File::Heap(Vec::new())),
            NodeKind::Directory => Node::Directory(Vec::new()),
        })?;
        self.attach(directory, name, node)?;
        Ok(node)
    }

    fn unlink(&mut self, path: Path<'_>) -> Result<(), FileSystemError> {
        let (directory, name) = self.parent_of(path)?;
        let (index, node) = self
            .locate(directory, name)?
            .ok_or(FileSystemError::NotFound)?;
        let info = self.metadata(node)?;
        if info.kind.is_directory() && info.size != 0 {
            return Err(FileSystemError::NotEmpty);
        }
        // Слот записи занимает надгробие: соседи по каталогу остаются на своих позициях.
        self.entries_mut(directory)?[index] = None;
        Ok(())
    }

    fn rename(&mut self, from: Path<'_>, to: Path<'_>) -> Result<(), FileSystemError> {
        let (source, source_name) = self.parent_of(from)?;
        let (index, node) = self
            .locate(source, source_name)?
            .ok_or(FileSystemError::NotFound)?;
        if from == to {
            return Ok(());
        }
        if self.metadata(node)?.kind.is_directory() && to.strip_prefix(from).is_some() {
            return Err(FileSystemError::InvalidPath);
        }

        let (target, target_name) = self.parent_of(to)?;
        if let Some((_, occupant)) = self.locate(target, target_name)?
            && self.metadata(occupant)?.kind.is_directory()
        {
            return Err(FileSystemError::IsADirectory);
        }
        self.attach(target, target_name, node)?;
        self.entries_mut(source)?[index] = None;
        Ok(())
    }

    fn metadata(&self, node: NodeId) -> Result<NodeInfo, FileSystemError> {
        Ok(match self.node(node)? {
            Node::File(File::Heap(data)) => NodeInfo {
                kind: NodeKind::File,
                size: data.len() as u64,
            },
            Node::File(File::Backed { len, .. }) => NodeInfo {
                kind: NodeKind::File,
                size: *len as u64,
            },
            Node::Directory(entries) => NodeInfo {
                kind: NodeKind::Directory,
                size: entries.iter().flatten().count() as u64,
            },
        })
    }

    fn read_at(
        &self,
        stores: &dyn ByteStore,
        node: NodeId,
        offset: u64,
        out: &mut [u8],
    ) -> Result<usize, FileSystemError> {
        let Ok(start) = usize::try_from(offset) else {
            return Ok(0);
        };
        match self.file_node(node)? {
            File::Heap(data) => {
                let Some(tail) = data.get(start..) else {
                    return Ok(0);
                };
                let count = tail.len().min(out.len());
                out[..count].copy_from_slice(&tail[..count]);
                Ok(count)
            }
            File::Backed { store, len, .. } => {
                let Some(count) = len.checked_sub(start).map(|tail| tail.min(out.len())) else {
                    return Ok(0);
                };
                stores.read(*store, start, &mut out[..count])?;
                Ok(count)
            }
        }
    }

    fn write_at(
        &mut self,
        stores: &mut dyn ByteStore,
        node: NodeId,
        offset: u64,
        data: &[u8],
    ) -> Result<usize, FileSystemError> {
        let start = usize::try_from(offset).map_err(|_| FileSystemError::NoSpace)?;
        let file = self.file_node_mut(node)?;
        let end = start
            .checked_add(data.len())
            .ok_or(FileSystemError::NoSpace)?;
        if end as u64 > FILE_SIZE_MAX {
            return Err(FileSystemError::NoSpace);
        }
        if matches!(file, File::Backed { capacity, .. } if end > *capacity) {
            let heap = heap_for_growth(file, stores)?;
            write_into(heap, start, data)?;
            return Ok(data.len());
        }
        match file {
            File::Heap(file) => write_into(file, start, data)?,
            File::Backed {
                store,
                len,
                capacity,
                ..
            } => {
                debug_assert!(end <= *capacity);
                if start > *len {
                    stores.zero(*store, *len, start - *len)?;
                }
                stores.write(*store, start, data)?;
                *len = (*len).max(end);
            }
        }
        Ok(data.len())
    }

    fn append(
        &mut self,
        stores: &mut dyn ByteStore,
        node: NodeId,
        data: &[u8],
    ) -> Result<u64, FileSystemError> {
        let offset = match self.file_node(node)? {
            File::Heap(file) => file.len(),
            File::Backed { len, .. } => *len,
        };
        self.write_at(stores, node, offset as u64, data)?;
        Ok(offset as u64)
    }

    fn set_size(
        &mut self,
        stores: &mut dyn ByteStore,
        node: NodeId,
        size: u64,
    ) -> Result<(), FileSystemError> {
        if size > FILE_SIZE_MAX {
            return Err(FileSystemError::NoSpace);
        }
        let len = usize::try_from(size).map_err(|_| FileSystemError::NoSpace)?;
        let file = self.file_node_mut(node)?;
        if matches!(file, File::Backed { capacity, .. } if len > *capacity) {
            return grow(heap_for_growth(file, stores)?, len);
        }
        match file {
            File::Heap(file) => {
                if len < file.len() {
                    file.truncate(len);
                    return Ok(());
                }
                grow(file, len)
            }
            File::Backed {
                store,
                len: current,
                capacity,
                ..
            } => {
                debug_assert!(len <= *capacity);
                if len > *current {
                    stores.zero(*store, *current, len - *current)?;
                }
                *current = len;
                Ok(())
            }
        }
    }

    fn list(
        &self,
        node: NodeId,
        from: DirCursor,
        out: &mut [u8],
    ) -> Result<Listed, FileSystemError> {
        let entries = self.entries(node)?;
        let start = from.raw() as usize;
        let mut written = 0;
        let mut count = 0;
        for (index, slot) in entries.iter().enumerate().skip(start) {
            let Some(entry) = slot else {
                continue;
            };
            let record = DirEntry {
                kind: self.metadata(entry.node)?.kind,
                name: &entry.name,
            };
            // Имя проверено при создании, и отказ значит нехватку места: слот
            // записи остаётся непросмотренным.
            let Ok(len) = record.encode(&mut out[written..]) else {
                return Ok(Listed {
                    count,
                    next: DirCursor::from_raw(index as u32),
                    end: false,
                });
            };
            written += len;
            count += 1;
        }
        // `from` за последним слотом не отматывает перечисление назад.
        Ok(Listed {
            count,
            next: DirCursor::from_raw(entries.len().max(start) as u32),
            end: true,
        })
    }

    fn backing(
        &mut self,
        stores: &mut dyn ByteStore,
        node: NodeId,
    ) -> Result<Window, FileSystemError> {
        let file = self.file_node_mut(node)?;
        if let File::Heap(data) = file {
            if data.is_empty() {
                return Err(FileSystemError::NotSupported);
            }
            let size = data.len();
            let capacity_u64 = (size as u64).div_ceil(USERLAND_PAGE_SIZE) * USERLAND_PAGE_SIZE;
            let capacity = usize::try_from(capacity_u64).map_err(|_| FileSystemError::NoSpace)?;
            let store = stores.allocate(capacity)?;
            if let Err(error) = stores.write(store, 0, data) {
                stores.release(store);
                return Err(error);
            }
            *file = File::Backed {
                store,
                len: size,
                capacity,
                pinned: false,
            };
        }
        match file {
            File::Heap(_) => unreachable!(),
            File::Backed {
                store,
                len,
                capacity,
                pinned: _,
            } if *len != 0 => Ok(Window {
                store: *store,
                offset: 0,
                size: *capacity as u64,
            }),
            File::Backed { .. } => Err(FileSystemError::NotSupported),
        }
    }

    fn pin(&mut self, node: NodeId) -> Result<(), FileSystemError> {
        match self.file_node_mut(node)? {
            File::Heap(_) => Err(FileSystemError::NotSupported),
            File::Backed { pinned, .. } => {
                *pinned = true;
                Ok(())
            }
        }
    }

    fn create_backed(
        &mut self,
        path: Path<'_>,
        store: StoreId,
        size: u64,
        capacity: u64,
    ) -> Result<NodeId, FileSystemError> {
        let len = usize::try_from(size).map_err(|_| FileSystemError::NoSpace)?;
        let capacity = usize::try_from(capacity).map_err(|_| FileSystemError::NoSpace)?;
        if size > FILE_SIZE_MAX || len > capacity {
            return Err(FileSystemError::NoSpace);
        }
        let (directory, name) = self.parent_of(path)?;
        if self.locate(directory, name)?.is_some() {
            return Err(FileSystemError::AlreadyExists);
        }
        let node = self.insert(Node::File(File::Backed {
            store,
            len,
            capacity,
            pinned: true,
        }))?;
        self.attach(directory, name, node)?;
        Ok(node)
    }
}

fn heap_for_growth<'a>(
    file: &'a mut File,
    stores: &mut dyn ByteStore,
) -> Result<&'a mut Vec<u8>, FileSystemError> {
    if let File::Backed {
        store, len, pinned, ..
    } = file
    {
        if *pinned {
            return Err(FileSystemError::NoSpace);
        }
        let (store, len) = (*store, *len);
        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(len)
            .map_err(|_| FileSystemError::NoSpace)?;
        bytes.resize(len, 0);
        stores.read(store, 0, &mut bytes)?;
        stores.release(store);
        *file = File::Heap(bytes);
    }
    let File::Heap(bytes) = file else {
        unreachable!()
    };
    Ok(bytes)
}

fn grow(file: &mut Vec<u8>, len: usize) -> Result<(), FileSystemError> {
    if len as u64 > FILE_SIZE_MAX {
        return Err(FileSystemError::NoSpace);
    }
    let Some(extra) = len.checked_sub(file.len()) else {
        return Ok(());
    };
    file.try_reserve(extra)
        .map_err(|_| FileSystemError::NoSpace)?;
    file.resize(len, 0);
    Ok(())
}

fn write_into(file: &mut Vec<u8>, start: usize, data: &[u8]) -> Result<(), FileSystemError> {
    let end = start
        .checked_add(data.len())
        .ok_or(FileSystemError::NoSpace)?;
    grow(file, end)?;
    file[start..end].copy_from_slice(data);
    Ok(())
}

#[cfg(test)]
mod tests {
    use alloc::{string::String, vec, vec::Vec};

    use filesystem_abi::{
        DirCursor, DirEntry, ENTRY_HEADER_SIZE, FileSystemError, Listed, NodeKind, Path,
    };
    use userland::USERLAND_PAGE_SIZE;

    use super::{FILE_SIZE_MAX, NODE_LIMIT, RamFileSystem as Backend};
    use crate::{
        ByteStore, HeapByteStore, backend::FileSystemBackend, node::NodeId, store::host::WithStore,
    };

    type RamFileSystem = WithStore<Backend>;

    impl RamFileSystem {
        fn new() -> Self {
            Self {
                backend: Backend::new(),
                stores: HeapByteStore::new(),
            }
        }
    }

    fn path(text: &str) -> Path<'_> {
        Path::new(text).expect("valid path")
    }

    fn cursor(slot: u32) -> DirCursor {
        DirCursor::from_raw(slot)
    }

    fn file(fs: &mut RamFileSystem, text: &str) -> NodeId {
        fs.create(path(text), NodeKind::File).expect("create file")
    }

    fn directory(fs: &mut RamFileSystem, text: &str) -> NodeId {
        fs.create(path(text), NodeKind::Directory)
            .expect("create directory")
    }

    fn read(fs: &RamFileSystem, node: NodeId, offset: u64, len: usize) -> Vec<u8> {
        let mut out = vec![0; len];
        let count = fs.read_at(node, offset, &mut out).expect("read");
        out.truncate(count);
        out
    }

    fn listed(count: u32, next: u32, end: bool) -> Listed {
        Listed {
            count,
            next: cursor(next),
            end,
        }
    }

    fn listing(
        fs: &RamFileSystem,
        node: NodeId,
        from: DirCursor,
        capacity: usize,
    ) -> (Vec<String>, Listed) {
        let mut out = vec![0; capacity];
        let listed = fs.list(node, from, &mut out).expect("list");
        let mut names = Vec::new();
        let mut offset = 0;
        for _ in 0..listed.count {
            let decoded = DirEntry::decode(&out[offset..]).expect("record");
            names.push(String::from(decoded.entry.name));
            offset += decoded.len;
        }
        (names, listed)
    }

    #[test]
    fn created_node_is_found_by_its_path() {
        let mut fs = RamFileSystem::new();
        let data = directory(&mut fs, "/data");
        let sample = file(&mut fs, "/data/sample");
        assert_eq!(fs.lookup(path("/data")), Ok(data));
        assert_eq!(fs.lookup(path("/data/sample")), Ok(sample));
        assert_eq!(fs.lookup(Path::ROOT), Ok(NodeId::from_raw(0)));
        assert_eq!(fs.metadata(sample).expect("metadata").kind, NodeKind::File);
    }

    #[test]
    fn create_of_existing_node_is_rejected() {
        let mut fs = RamFileSystem::new();
        file(&mut fs, "/lock");
        assert_eq!(
            fs.create(path("/lock"), NodeKind::File),
            Err(FileSystemError::AlreadyExists)
        );
        assert_eq!(
            fs.create(path("/lock"), NodeKind::Directory),
            Err(FileSystemError::AlreadyExists)
        );
    }

    #[test]
    fn lookup_of_missing_name_is_not_found() {
        let mut fs = RamFileSystem::new();
        directory(&mut fs, "/data");
        assert_eq!(
            fs.lookup(path("/data/sample")),
            Err(FileSystemError::NotFound)
        );
        assert_eq!(fs.lookup(path("/usr/bin")), Err(FileSystemError::NotFound));
    }

    #[test]
    fn descent_through_a_file_is_not_a_directory() {
        let mut fs = RamFileSystem::new();
        file(&mut fs, "/sample");
        assert_eq!(
            fs.lookup(path("/sample/inner")),
            Err(FileSystemError::NotADirectory)
        );
        assert_eq!(
            fs.create(path("/sample/inner"), NodeKind::File),
            Err(FileSystemError::NotADirectory)
        );
    }

    #[test]
    fn create_of_the_root_is_rejected() {
        let mut fs = RamFileSystem::new();
        assert_eq!(
            fs.create(Path::ROOT, NodeKind::Directory),
            Err(FileSystemError::InvalidPath)
        );
        assert_eq!(fs.unlink(Path::ROOT), Err(FileSystemError::InvalidPath));
    }

    #[test]
    fn read_at_offset_and_past_the_end() {
        let mut fs = RamFileSystem::new();
        let sample = file(&mut fs, "/sample");
        fs.write_at(sample, 0, b"hello world").expect("write");
        assert_eq!(read(&fs, sample, 0, 5), b"hello");
        assert_eq!(read(&fs, sample, 6, 5), b"world");
        assert_eq!(read(&fs, sample, 6, 64), b"world");
        assert_eq!(read(&fs, sample, 11, 4), b"");
        assert_eq!(read(&fs, sample, 64, 4), b"");
    }

    #[test]
    fn write_past_the_end_grows_the_file_with_zeros() {
        let mut fs = RamFileSystem::new();
        let sample = file(&mut fs, "/sample");
        assert_eq!(fs.write_at(sample, 4, b"tail"), Ok(4));
        assert_eq!(fs.metadata(sample).expect("metadata").size, 8);
        assert_eq!(read(&fs, sample, 0, 8), b"\0\0\0\0tail");

        assert_eq!(fs.write_at(sample, 0, b"head"), Ok(4));
        assert_eq!(read(&fs, sample, 0, 8), b"headtail");
        assert_eq!(fs.metadata(sample).expect("metadata").size, 8);
    }

    #[test]
    fn append_returns_the_offset_of_the_written_bytes() {
        let mut fs = RamFileSystem::new();
        let log = file(&mut fs, "/log");
        assert_eq!(fs.append(log, b"one"), Ok(0));
        assert_eq!(fs.append(log, b"two"), Ok(3));
        assert_eq!(fs.append(log, b""), Ok(6));
        assert_eq!(read(&fs, log, 0, 16), b"onetwo");
    }

    #[test]
    fn set_size_truncates_and_extends() {
        let mut fs = RamFileSystem::new();
        let sample = file(&mut fs, "/sample");
        fs.write_at(sample, 0, b"hello world").expect("write");

        assert_eq!(fs.set_size(sample, 5), Ok(()));
        assert_eq!(read(&fs, sample, 0, 16), b"hello");

        assert_eq!(fs.set_size(sample, 8), Ok(()));
        assert_eq!(fs.metadata(sample).expect("metadata").size, 8);
        assert_eq!(read(&fs, sample, 0, 16), b"hello\0\0\0");

        assert_eq!(fs.set_size(sample, 0), Ok(()));
        assert_eq!(read(&fs, sample, 0, 16), b"");
    }

    #[test]
    fn length_past_the_ceiling_is_refused() {
        let mut fs = RamFileSystem::new();
        let sample = file(&mut fs, "/sample");
        fs.write_at(sample, 0, b"body").expect("write");

        for size in [FILE_SIZE_MAX + 1, 1 << 40, u64::MAX] {
            assert_eq!(fs.set_size(sample, size), Err(FileSystemError::NoSpace));
            assert_eq!(
                fs.write_at(sample, size, b"x"),
                Err(FileSystemError::NoSpace)
            );
            assert_eq!(
                fs.write_at(sample, size - 1, b"xx"),
                Err(FileSystemError::NoSpace)
            );
        }
        // Отказ не тронул содержимое узла.
        assert_eq!(read(&fs, sample, 0, 16), b"body");
    }

    #[test]
    fn file_grows_up_to_the_ceiling_and_no_further() {
        let mut fs = RamFileSystem::new();
        let sample = file(&mut fs, "/sample");

        assert_eq!(fs.set_size(sample, FILE_SIZE_MAX), Ok(()));
        assert_eq!(fs.metadata(sample).expect("metadata").size, FILE_SIZE_MAX);
        assert_eq!(fs.append(sample, b"x"), Err(FileSystemError::NoSpace));
        assert_eq!(
            fs.write_at(sample, FILE_SIZE_MAX, b"x"),
            Err(FileSystemError::NoSpace)
        );
        // Запись в пределах занятой длины потолка не касается.
        assert_eq!(fs.write_at(sample, FILE_SIZE_MAX - 1, b"x"), Ok(1));

        // Усечение доступно с любой длины.
        assert_eq!(fs.set_size(sample, 0), Ok(()));
        assert_eq!(fs.metadata(sample).expect("metadata").size, 0);
    }

    #[test]
    fn unlink_removes_the_name_and_keeps_the_node() {
        let mut fs = RamFileSystem::new();
        let sample = file(&mut fs, "/sample");
        fs.write_at(sample, 0, b"body").expect("write");

        assert_eq!(fs.unlink(path("/sample")), Ok(()));
        assert_eq!(fs.lookup(path("/sample")), Err(FileSystemError::NotFound));
        assert_eq!(fs.unlink(path("/sample")), Err(FileSystemError::NotFound));
        // Дескриптор закрепляет идентичность: узел переживает имя.
        assert_eq!(read(&fs, sample, 0, 16), b"body");
    }

    #[test]
    fn unlink_of_a_non_empty_directory_is_refused() {
        let mut fs = RamFileSystem::new();
        let data = directory(&mut fs, "/data");
        file(&mut fs, "/data/sample");

        assert_eq!(fs.unlink(path("/data")), Err(FileSystemError::NotEmpty));
        // Отказ не тронул ни имя каталога, ни его содержимое.
        assert_eq!(fs.lookup(path("/data")), Ok(data));
        assert_eq!(listing(&fs, data, DirCursor::START, 64).0, ["sample"]);
        assert!(fs.lookup(path("/data/sample")).is_ok());
    }

    #[test]
    fn unlink_of_an_emptied_directory_is_allowed() {
        let mut fs = RamFileSystem::new();
        let data = directory(&mut fs, "/data");
        file(&mut fs, "/data/sample");

        fs.unlink(path("/data/sample")).expect("unlink the entry");
        // Надгробие не считается записью: каталог пуст.
        assert_eq!(fs.metadata(data).expect("metadata").size, 0);
        assert_eq!(fs.unlink(path("/data")), Ok(()));
        assert_eq!(fs.lookup(path("/data")), Err(FileSystemError::NotFound));
    }

    #[test]
    fn rename_moves_the_node_between_directories() {
        let mut fs = RamFileSystem::new();
        directory(&mut fs, "/data");
        directory(&mut fs, "/store");
        let sample = file(&mut fs, "/data/sample");

        assert_eq!(
            fs.rename(path("/data/sample"), path("/store/sample")),
            Ok(())
        );
        assert_eq!(
            fs.lookup(path("/data/sample")),
            Err(FileSystemError::NotFound)
        );
        assert_eq!(fs.lookup(path("/store/sample")), Ok(sample));
    }

    #[test]
    fn rename_replaces_an_occupied_file_name() {
        let mut fs = RamFileSystem::new();
        let fresh = file(&mut fs, "/sample.new");
        let stale = file(&mut fs, "/sample");
        fs.write_at(fresh, 0, b"fresh").expect("write");

        assert_eq!(fs.rename(path("/sample.new"), path("/sample")), Ok(()));
        assert_eq!(fs.lookup(path("/sample")), Ok(fresh));
        assert_eq!(
            fs.lookup(path("/sample.new")),
            Err(FileSystemError::NotFound)
        );
        assert_eq!(read(&fs, stale, 0, 16), b"");
    }

    #[test]
    fn rename_keeps_the_slot_of_the_replaced_name() {
        let mut fs = RamFileSystem::new();
        let data = directory(&mut fs, "/data");
        for name in ["a", "b", "c"] {
            file(&mut fs, &(String::from("/data/") + name));
        }

        fs.rename(path("/data/c"), path("/data/b")).expect("rename");
        // Замещённое имя осталось в своём слоте, надгробие легло на источник.
        assert_eq!(listing(&fs, data, DirCursor::START, 64).0, ["a", "b"]);
        assert_eq!(listing(&fs, data, cursor(1), 64).0, ["b"]);
        assert!(listing(&fs, data, cursor(2), 64).0.is_empty());
    }

    #[test]
    fn rename_onto_a_directory_is_rejected() {
        let mut fs = RamFileSystem::new();
        file(&mut fs, "/sample");
        directory(&mut fs, "/data");
        assert_eq!(
            fs.rename(path("/sample"), path("/data")),
            Err(FileSystemError::IsADirectory)
        );
    }

    #[test]
    fn rename_of_a_missing_source_is_not_found() {
        let mut fs = RamFileSystem::new();
        assert_eq!(
            fs.rename(path("/absent"), path("/target")),
            Err(FileSystemError::NotFound)
        );
    }

    #[test]
    fn rename_of_a_directory_into_itself_is_rejected() {
        let mut fs = RamFileSystem::new();
        directory(&mut fs, "/data");
        directory(&mut fs, "/data/inner");
        assert_eq!(
            fs.rename(path("/data"), path("/data/inner/data")),
            Err(FileSystemError::InvalidPath)
        );
        assert_eq!(fs.rename(path("/data"), path("/data")), Ok(()));
        assert!(fs.lookup(path("/data/inner")).is_ok());
    }

    #[test]
    fn rename_of_a_missing_source_onto_itself_is_not_found() {
        let mut fs = RamFileSystem::new();
        assert_eq!(
            fs.rename(path("/absent"), path("/absent")),
            Err(FileSystemError::NotFound)
        );
    }

    #[test]
    fn list_fills_the_buffer_in_batches() {
        let mut fs = RamFileSystem::new();
        let data = directory(&mut fs, "/data");
        for name in ["a", "b", "c"] {
            file(&mut fs, &(String::from("/data/") + name));
        }

        let room = 2 * (ENTRY_HEADER_SIZE + 1);
        let (names, first) = listing(&fs, data, DirCursor::START, room);
        assert_eq!(names, ["a", "b"]);
        assert_eq!(first, listed(2, 2, false));

        let (names, second) = listing(&fs, data, first.next, room);
        assert_eq!(names, ["c"]);
        assert_eq!(second, listed(1, 3, true));
    }

    #[test]
    fn slot_indices_are_stable_across_unlink() {
        let mut fs = RamFileSystem::new();
        let data = directory(&mut fs, "/data");
        for name in ["a", "b", "c"] {
            file(&mut fs, &(String::from("/data/") + name));
        }
        fs.unlink(path("/data/b")).expect("unlink");
        file(&mut fs, "/data/d");

        // Соседи надгробия не сдвинулись, а новая запись переиспользовала его слот.
        assert_eq!(listing(&fs, data, DirCursor::START, 64).0, ["a", "d", "c"]);
        assert_eq!(listing(&fs, data, cursor(2), 64).0, ["c"]);
        assert_eq!(fs.metadata(data).expect("metadata").size, 3);
    }

    #[test]
    fn node_count_is_capped() {
        let mut fs = RamFileSystem::new();
        // Корень уже занимает узел арены.
        for index in 1..NODE_LIMIT {
            file(&mut fs, &(String::from("/n") + &index.to_string()));
        }
        assert_eq!(
            fs.create(path("/one-too-many"), NodeKind::File),
            Err(FileSystemError::NoSpace)
        );
        // Потолок отказывает только росту: существующие узлы живы.
        assert!(fs.lookup(path("/n1")).is_ok());
    }

    #[test]
    fn rename_cycle_reuses_tombstones() {
        let mut fs = RamFileSystem::new();
        let data = directory(&mut fs, "/data");
        file(&mut fs, "/data/a");
        for _ in 0..8 {
            fs.rename(path("/data/a"), path("/data/b")).expect("rename");
            fs.rename(path("/data/b"), path("/data/a")).expect("rename");
        }

        // Имя ходит между двумя слотами, каталог за них не растёт.
        assert_eq!(fs.metadata(data).expect("metadata").size, 1);
        assert!(listing(&fs, data, cursor(2), 64).0.is_empty());
    }

    #[test]
    fn every_entry_is_listed_once_across_batches_over_tombstones() {
        let mut fs = RamFileSystem::new();
        let data = directory(&mut fs, "/data");
        for name in ["a", "b", "c", "d", "e"] {
            file(&mut fs, &(String::from("/data/") + name));
        }
        fs.unlink(path("/data/b")).expect("unlink");

        // Надгробие в слоте 1 занимает слот, не давая записи: первый заход из
        // двух записей доходит до слота 3 вместо слота 2.
        let room = 2 * (ENTRY_HEADER_SIZE + 1);
        let mut seen = Vec::new();
        let mut from = DirCursor::START;
        loop {
            let (names, batch) = listing(&fs, data, from, room);
            assert_ne!(batch.count, 0, "batch made no progress");
            seen.extend(names);
            if batch.end {
                break;
            }
            from = batch.next;
        }
        assert_eq!(seen, ["a", "c", "d", "e"]);
        assert_eq!(
            listing(&fs, data, DirCursor::START, room).1,
            listed(2, 3, false)
        );
    }

    #[test]
    fn list_into_a_buffer_shorter_than_a_record_writes_nothing() {
        let mut fs = RamFileSystem::new();
        let data = directory(&mut fs, "/data");
        file(&mut fs, "/data/a");

        // Запись не помещается: перечисление стоит на своём слоте, и повтор с
        // тем же буфером не сдвинет его.
        let (names, batch) = listing(&fs, data, DirCursor::START, ENTRY_HEADER_SIZE);
        assert!(names.is_empty());
        assert_eq!(batch, listed(0, 0, false));
        assert_eq!(listing(&fs, data, batch.next, 0).1, listed(0, 0, false));
    }

    #[test]
    fn list_past_the_last_slot_is_empty() {
        let mut fs = RamFileSystem::new();
        let data = directory(&mut fs, "/data");
        file(&mut fs, "/data/a");
        assert_eq!(listing(&fs, data, cursor(7), 64).1, listed(0, 7, true));
    }

    #[test]
    fn kinds_of_the_listed_entries_are_reported() {
        let mut fs = RamFileSystem::new();
        let data = directory(&mut fs, "/data");
        file(&mut fs, "/data/sample");
        directory(&mut fs, "/data/rc.d");

        let mut out = vec![0; 64];
        fs.list(data, DirCursor::START, &mut out).expect("list");
        let sample = DirEntry::decode(&out).expect("record");
        let rc = DirEntry::decode(&out[sample.len..]).expect("record");
        assert_eq!(sample.entry.kind, NodeKind::File);
        assert_eq!(rc.entry.kind, NodeKind::Directory);
    }

    #[test]
    fn file_and_directory_operations_do_not_mix() {
        let mut fs = RamFileSystem::new();
        let data = directory(&mut fs, "/data");
        let sample = file(&mut fs, "/data/sample");

        assert_eq!(
            fs.read_at(data, 0, &mut [0; 4]),
            Err(FileSystemError::IsADirectory)
        );
        assert_eq!(
            fs.write_at(data, 0, b"x"),
            Err(FileSystemError::IsADirectory)
        );
        assert_eq!(fs.append(data, b"x"), Err(FileSystemError::IsADirectory));
        assert_eq!(fs.set_size(data, 0), Err(FileSystemError::IsADirectory));
        assert_eq!(
            fs.list(sample, DirCursor::START, &mut [0; 16]),
            Err(FileSystemError::NotADirectory)
        );
    }

    #[test]
    fn empty_file_has_no_backing() {
        let mut fs = RamFileSystem::new();
        let sample = file(&mut fs, "/sample");
        assert_eq!(fs.backing(sample), Err(FileSystemError::NotSupported));
    }

    #[test]
    fn nonempty_file_is_promoted_and_shares_its_bytes() {
        let mut fs = RamFileSystem::new();
        let sample = file(&mut fs, "/sample");
        fs.write_at(sample, 0, b"before").expect("write");

        let window = fs.backing(sample).expect("promote");
        assert_eq!(window.offset, 0);
        assert_eq!(window.size, USERLAND_PAGE_SIZE);
        let mut bytes = [0; 6];
        fs.stores
            .read(window.store, 0, &mut bytes)
            .expect("read store");
        assert_eq!(&bytes, b"before");

        fs.stores
            .write(window.store, 0, b"mapped")
            .expect("write store");
        assert_eq!(read(&fs, sample, 0, 6), b"mapped");

        fs.write_at(sample, 0, b"server").expect("write backend");
        fs.stores
            .read(window.store, 0, &mut bytes)
            .expect("read store");
        assert_eq!(&bytes, b"server");
    }

    #[test]
    fn issued_backing_pins_capacity() {
        let mut fs = RamFileSystem::new();
        let sample = file(&mut fs, "/sample");
        fs.write_at(sample, 0, b"body").expect("write");
        fs.backing(sample).expect("promote");
        fs.pin(sample).expect("pin");

        assert_eq!(
            fs.write_at(sample, USERLAND_PAGE_SIZE, b"x"),
            Err(FileSystemError::NoSpace)
        );
        assert_eq!(
            fs.set_size(sample, USERLAND_PAGE_SIZE + 1),
            Err(FileSystemError::NoSpace)
        );
        assert_eq!(read(&fs, sample, 0, 8), b"body");
        fs.set_size(sample, USERLAND_PAGE_SIZE)
            .expect("fill capacity");
        assert_eq!(fs.append(sample, b"x"), Err(FileSystemError::NoSpace));
    }

    #[test]
    fn unissued_backing_can_return_to_the_heap_and_grow() {
        let mut fs = RamFileSystem::new();
        let sample = file(&mut fs, "/sample");
        fs.write_at(sample, 0, b"body").expect("write");
        fs.backing(sample).expect("promote");

        assert_eq!(fs.write_at(sample, USERLAND_PAGE_SIZE, b"x"), Ok(1));
        assert_eq!(fs.metadata(sample).expect("metadata").size, 4097);
        assert_eq!(read(&fs, sample, 0, 4), b"body");
        assert_eq!(read(&fs, sample, USERLAND_PAGE_SIZE, 1), b"x");
    }

    #[test]
    fn file_can_be_created_over_an_existing_store() {
        let mut fs = RamFileSystem::new();
        let store = fs
            .stores
            .allocate(USERLAND_PAGE_SIZE as usize)
            .expect("allocate store");
        fs.stores
            .write(store, 0, b"shared")
            .expect("initialize store");
        let sample = fs
            .backend
            .create_backed(path("/shared"), store, 6, USERLAND_PAGE_SIZE)
            .expect("create backed file");

        assert_eq!(read(&fs, sample, 0, 6), b"shared");
        fs.stores.write(store, 0, b"mapped").expect("change store");
        assert_eq!(read(&fs, sample, 0, 6), b"mapped");
        assert_eq!(
            fs.write_at(sample, USERLAND_PAGE_SIZE, b"x"),
            Err(FileSystemError::NoSpace)
        );
    }
}
