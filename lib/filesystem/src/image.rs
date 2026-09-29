//! Read-only дерево поверх плоской таблицы образа: записи отсортированы по
//! пути: поиск узла - двоичный, содержимое каталога - непрерывный
//! диапазон записей с общим префиксом.
//!
//! Дерево накрывает образ целиком: корень дерева есть корень образа, и путь
//! записи (`/boot/init`) он же путь узла.

use alloc::vec::Vec;
use core::{cmp::Ordering, iter};

use filesystem_abi::{
    DirCursor, DirEntry, FileSystemError, Listed, NodeInfo, NodeKind, PATH_MAX, Path,
};
use userland_image::{ImageDecodeError, decode};

use crate::{
    backend::{FileSystemBackend, kind_mismatch},
    node::{NodeId, Window},
    store::{ByteStore, StoreId},
};

const SEPARATOR: char = '/';

const ROOT: NodeId = NodeId::from_raw(0);

const DEPTH_BITS: u32 = 8;
const DEPTH_MASK: u32 = (1 << DEPTH_BITS) - 1;

const _: () = assert!(
    // Компонент вместе с разделителем занимает не меньше двух байт пути.
    PATH_MAX / 2 <= DEPTH_MASK as usize
);

const _: () = assert!(userland_image::USERLAND_ENTRY_PATH_CAPACITY <= PATH_MAX);

/// Отказ построения дерева по образу.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageFileSystemError {
    /// Образ не разобран кодеком.
    Decode(ImageDecodeError),
    /// Путь записи не годится в путь контракта.
    EntryPath { index: usize },
    /// Образ не содержит ни одной записи.
    EmptyImage,
}

struct Record {
    path: Path<'static>,
    data: &'static [u8],
    offset: u64,
}

#[derive(Clone, Copy)]
struct Located<'a> {
    record: &'a Record,
    index: usize,
    depth: usize,
}

/// Дерево имён поверх таблицы образа. Каталог существует, пока его называет
/// путь хотя бы одной записи, и дерево обходится без отдельной структуры под
/// каталоги.
pub struct ImageFileSystem {
    entries: Vec<Record>,
}

impl ImageFileSystem {
    /// Строит дерево из записей образа. Возвращает `EntryPath`, если путь нарушает правила
    /// контракта, и `EmptyImage`, если образ не содержит записей.
    pub fn new(image: &'static [u8]) -> Result<Self, ImageFileSystemError> {
        let decoded = decode(image).map_err(ImageFileSystemError::Decode)?;
        let mut entries = Vec::new();
        for (index, entry) in decoded.entries().enumerate() {
            let path =
                Path::new(entry.path()).map_err(|_| ImageFileSystemError::EntryPath { index })?;
            entries.push(Record {
                path,
                data: entry.data(),
                offset: entry.data_offset(),
            });
        }
        if entries.is_empty() {
            return Err(ImageFileSystemError::EmptyImage);
        }
        Ok(Self { entries })
    }

    fn locate(&self, node: NodeId) -> Result<Located<'_>, FileSystemError> {
        let index = (node.raw() >> DEPTH_BITS) as usize;
        let depth = (node.raw() & DEPTH_MASK) as usize;
        let record = self.entries.get(index).ok_or(FileSystemError::NotFound)?;
        if depth > record.path.components().count() {
            return Err(FileSystemError::NotFound);
        }
        Ok(Located {
            record,
            index,
            depth,
        })
    }

    fn entries_under<'a>(
        &'a self,
        prefix: &'a str,
        from: usize,
    ) -> impl Iterator<Item = (usize, DirEntry<'a>)> {
        let mut listed: Option<&str> = None;
        self.entries
            .get(from..)
            .unwrap_or_default()
            .iter()
            .enumerate()
            .map_while(move |(offset, record)| {
                Some((from + offset, entry_under(record.path.as_str(), prefix)?))
            })
            // Соседние записи каталога с общим первым компонентом дают его имя один раз.
            .filter(move |(_, entry)| listed.replace(entry.name) != Some(entry.name))
    }

    fn range_end(&self, prefix: &str, from: usize) -> usize {
        from + self
            .entries
            .get(from..)
            .unwrap_or_default()
            .iter()
            .take_while(|record| entry_under(record.path.as_str(), prefix).is_some())
            .count()
    }

    fn search(&self, path: Path<'_>) -> Result<usize, usize> {
        self.entries
            .binary_search_by(|record| record.path.as_str().cmp(path.as_str()))
    }

    fn missing(&self, path: Path<'_>) -> FileSystemError {
        let mut current = path;
        while let Some((parent, _)) = current.split_last() {
            if self.search(parent).is_ok() {
                return FileSystemError::NotADirectory;
            }
            current = parent;
        }
        FileSystemError::NotFound
    }

    fn refuse_change(&self, node: NodeId) -> FileSystemError {
        self.locate(node)
            .and_then(Located::data)
            .err()
            .unwrap_or(FileSystemError::ReadOnly)
    }

    fn parent_directory(&self, path: Path<'_>) -> Result<(), FileSystemError> {
        let (parent, _) = path.split_last().ok_or(FileSystemError::InvalidPath)?;
        if self.locate(self.lookup(parent)?)?.is_file() {
            return Err(FileSystemError::NotADirectory);
        }
        Ok(())
    }

    fn locate_path(&self, path: Path<'_>) -> Result<Located<'_>, FileSystemError> {
        self.parent_directory(path)?;
        self.locate(self.lookup(path)?)
    }

    fn refuse_new_name(&self, path: Path<'_>) -> FileSystemError {
        if let Err(error) = self.parent_directory(path) {
            return error;
        }
        if self.lookup(path).is_ok() {
            return FileSystemError::AlreadyExists;
        }
        FileSystemError::ReadOnly
    }

    fn refuse_rename(&self, from: Path<'_>, to: Path<'_>) -> FileSystemError {
        if let Err(error) = self.locate_path(from) {
            return error;
        }
        if let Err(error) = self.parent_directory(to) {
            return error;
        }
        // Занятое имя перенос замещает, каталог - нет. Свободное имя упирается
        // в режим монтирования.
        match self.lookup(to).and_then(|node| self.locate(node)) {
            Ok(node) if !node.is_file() => FileSystemError::IsADirectory,
            _ => FileSystemError::ReadOnly,
        }
    }
}

impl<'a> Located<'a> {
    fn prefix_len(self) -> usize {
        self.record
            .path
            .components()
            .take(self.depth)
            .map(|component| component.as_str().len() + 1)
            .sum()
    }

    fn is_file(self) -> bool {
        self.prefix_len() == self.record.path.as_str().len()
    }

    fn kind(self) -> NodeKind {
        if self.is_file() {
            NodeKind::File
        } else {
            NodeKind::Directory
        }
    }

    fn prefix(self) -> &'a str {
        &self.record.path.as_str()[..self.prefix_len()]
    }

    fn data(self) -> Result<&'a [u8], FileSystemError> {
        self.is_file()
            .then_some(self.record.data)
            .ok_or(FileSystemError::IsADirectory)
    }
}

impl FileSystemBackend for ImageFileSystem {
    fn is_mutable(&self) -> bool {
        false
    }

    fn lookup(&self, path: Path<'_>) -> Result<NodeId, FileSystemError> {
        if path.is_root() {
            return Ok(ROOT);
        }
        let depth = path.components().count();
        if let Ok(index) = self.search(path) {
            return Ok(node_id(index, depth));
        }
        // Между путём каталога и его содержимым лежат имена, начинающиеся с
        // байта меньше разделителя ("/boot.old" перед "/boot/init"). Границу
        // задаёт путь каталога вместе с разделителем.
        let index = self
            .entries
            .partition_point(|record| precedes_content(record.path.as_str(), path.as_str()));
        match self.entries.get(index) {
            Some(record) if entry_under(record.path.as_str(), path.as_str()).is_some() => {
                Ok(node_id(index, depth))
            }
            _ => Err(self.missing(path)),
        }
    }

    fn create(&mut self, path: Path<'_>, _kind: NodeKind) -> Result<NodeId, FileSystemError> {
        Err(self.refuse_new_name(path))
    }

    fn unlink(
        &mut self,
        _stores: &mut dyn ByteStore,
        path: Path<'_>,
        expected: NodeKind,
    ) -> Result<(), FileSystemError> {
        let node = self.locate_path(path)?;
        if node.kind() != expected {
            return Err(kind_mismatch(expected));
        }
        Err(FileSystemError::ReadOnly)
    }

    fn rename(
        &mut self,
        _stores: &mut dyn ByteStore,
        from: Path<'_>,
        to: Path<'_>,
    ) -> Result<(), FileSystemError> {
        Err(self.refuse_rename(from, to))
    }

    fn retain(&mut self, node: NodeId) -> Result<(), FileSystemError> {
        self.locate(node)?;
        Ok(())
    }

    fn release(&mut self, _stores: &mut dyn ByteStore, _node: NodeId) {
        // Узлы дерева выведены из таблицы образа. Держателей она не считает.
    }

    fn metadata(&self, node: NodeId) -> Result<NodeInfo, FileSystemError> {
        let node = self.locate(node)?;
        if node.is_file() {
            return Ok(NodeInfo {
                kind: NodeKind::File,
                size: node.record.data.len() as u64,
            });
        }
        Ok(NodeInfo {
            kind: NodeKind::Directory,
            size: self.entries_under(node.prefix(), node.index).count() as u64,
        })
    }

    fn read_at(
        &self,
        _stores: &dyn ByteStore,
        node: NodeId,
        offset: u64,
        out: &mut [u8],
    ) -> Result<usize, FileSystemError> {
        let data = self.locate(node)?.data()?;
        let Ok(start) = usize::try_from(offset) else {
            return Ok(0);
        };
        let Some(tail) = data.get(start..) else {
            return Ok(0);
        };
        let count = tail.len().min(out.len());
        let (Some(target), Some(source)) = (out.get_mut(..count), tail.get(..count)) else {
            return Ok(0);
        };
        target.copy_from_slice(source);
        Ok(count)
    }

    fn write_at(
        &mut self,
        _stores: &mut dyn ByteStore,
        node: NodeId,
        _offset: u64,
        _data: &[u8],
    ) -> Result<usize, FileSystemError> {
        Err(self.refuse_change(node))
    }

    fn append(
        &mut self,
        _stores: &mut dyn ByteStore,
        node: NodeId,
        _data: &[u8],
    ) -> Result<u64, FileSystemError> {
        Err(self.refuse_change(node))
    }

    fn set_size(
        &mut self,
        _stores: &mut dyn ByteStore,
        node: NodeId,
        _size: u64,
    ) -> Result<(), FileSystemError> {
        Err(self.refuse_change(node))
    }

    fn list(
        &self,
        node: NodeId,
        from: DirCursor,
        out: &mut [u8],
    ) -> Result<Listed, FileSystemError> {
        let node = self.locate(node)?;
        if node.is_file() {
            return Err(FileSystemError::NotADirectory);
        }
        let prefix = node.prefix();
        // Позиция перед первой записью каталога не отматывает перечисление к
        // соседям, лежащим в таблице раньше.
        let start = (from.raw() as usize).max(node.index);
        let mut written = 0;
        let mut count = 0;
        for (index, entry) in self.entries_under(prefix, start) {
            // Имя проверено при разборе образа, и отказ значит нехватку
            // места: запись остаётся непросмотренной.
            let cursor = u32::try_from(index).map_err(|_| FileSystemError::NoSpace)?;
            let Some(rest) = out.get_mut(written..) else {
                return Ok(Listed {
                    count,
                    next: DirCursor::from_raw(cursor),
                    end: false,
                });
            };
            let Ok(len) = entry.encode(rest) else {
                return Ok(Listed {
                    count,
                    next: DirCursor::from_raw(cursor),
                    end: false,
                });
            };
            written += len;
            count += 1;
        }
        let end =
            u32::try_from(self.range_end(prefix, start)).map_err(|_| FileSystemError::NoSpace)?;
        Ok(Listed {
            count,
            next: DirCursor::from_raw(end),
            end: true,
        })
    }

    fn backing(
        &mut self,
        _stores: &mut dyn ByteStore,
        node: NodeId,
    ) -> Result<Window, FileSystemError> {
        let node = self.locate(node)?;
        let size = node.data()?.len() as u64;
        if size == 0 {
            return Err(FileSystemError::NotSupported);
        }
        Ok(Window {
            store: StoreId::MOUNT,
            offset: node.record.offset,
            size,
        })
    }

    fn pin(&mut self, node: NodeId) -> Result<(), FileSystemError> {
        self.locate(node)?;
        Ok(())
    }
}

fn node_id(index: usize, depth: usize) -> NodeId {
    // Записей в образе не больше u16::MAX: пара (номер, глубина) укладывается в NodeId.
    #[allow(clippy::cast_possible_truncation)]
    let packed = ((index as u32) << DEPTH_BITS) | depth as u32;
    NodeId::from_raw(packed)
}

fn entry_under<'a>(path: &'a str, prefix: &str) -> Option<DirEntry<'a>> {
    let rest = path.strip_prefix(prefix)?.strip_prefix(SEPARATOR)?;
    Some(match rest.find(SEPARATOR) {
        Some(cut) => DirEntry {
            kind: NodeKind::Directory,
            name: &rest[..cut],
        },
        None => DirEntry {
            kind: NodeKind::File,
            name: rest,
        },
    })
}

fn precedes_content(path: &str, prefix: &str) -> bool {
    path.chars()
        .cmp(prefix.chars().chain(iter::once(SEPARATOR)))
        == Ordering::Less
}

#[cfg(test)]
mod tests {
    use alloc::{string::String, vec, vec::Vec};
    use std::sync::OnceLock;

    use filesystem_abi::{
        DirCursor, DirEntry, ENTRY_HEADER_SIZE, FileSystemError, Listed, NodeKind, Path,
    };
    use userland_image::{ImageRecord, encode};

    use super::{ImageFileSystem as Backend, ImageFileSystemError};
    use crate::{
        HeapByteStore,
        backend::FileSystemBackend,
        node::{NodeId, Window},
        store::host::WithStore,
    };

    const SAMPLE: &[u8] = b"hello from the image";

    type ImageFileSystem = WithStore<Backend>;

    impl ImageFileSystem {
        fn new(image: &'static [u8]) -> Result<Self, ImageFileSystemError> {
            Ok(Self {
                backend: Backend::new(image)?,
                stores: HeapByteStore::new(),
            })
        }
    }

    fn path(text: &str) -> Path<'_> {
        Path::new(text).expect("valid path")
    }

    fn cursor(slot: u32) -> DirCursor {
        DirCursor::from_raw(slot)
    }

    fn image_bytes() -> &'static [u8] {
        static BYTES: OnceLock<Vec<u8>> = OnceLock::new();
        BYTES.get_or_init(|| {
            encode(
                &[
                    ImageRecord {
                        path: "/boot/init",
                        data: b"init",
                    },
                    ImageRecord {
                        path: "/boot/lib/hello",
                        data: b"hello",
                    },
                    ImageRecord {
                        path: "/boot/lib/world",
                        data: b"world",
                    },
                    ImageRecord {
                        path: "/boot.old",
                        data: b"stale",
                    },
                    ImageRecord {
                        path: "/data/sample",
                        data: SAMPLE,
                    },
                ],
                "/boot/init",
            )
            .expect("encode image")
        })
    }

    fn image() -> ImageFileSystem {
        ImageFileSystem::new(image_bytes()).expect("decode image")
    }

    fn image_from(records: &[ImageRecord<'_>]) -> Result<ImageFileSystem, ImageFileSystemError> {
        let bytes = encode(records, records[0].path).expect("encode image");
        // Байты остаются в памяти до конца процесса, как регион образа в сервере.
        ImageFileSystem::new(Vec::leak(bytes))
    }

    fn node(fs: &ImageFileSystem, text: &str) -> NodeId {
        fs.lookup(path(text)).expect("lookup")
    }

    fn read(fs: &ImageFileSystem, node: NodeId, offset: u64, len: usize) -> Vec<u8> {
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
        fs: &ImageFileSystem,
        node: NodeId,
        from: DirCursor,
        capacity: usize,
    ) -> (Vec<(String, NodeKind)>, Listed) {
        let mut out = vec![0; capacity];
        let listed = fs.list(node, from, &mut out).expect("list");
        let mut entries = Vec::new();
        let mut offset = 0;
        for _ in 0..listed.count {
            let decoded = DirEntry::decode(&out[offset..]).expect("record");
            entries.push((String::from(decoded.entry.name), decoded.entry.kind));
            offset += decoded.len;
        }
        (entries, listed)
    }

    fn names(entries: &[(String, NodeKind)]) -> Vec<&str> {
        entries.iter().map(|(name, _)| name.as_str()).collect()
    }

    #[test]
    fn paths_of_the_table_resolve_to_files_and_directories() {
        let fs = image();
        assert_eq!(
            fs.metadata(node(&fs, "/data/sample"))
                .expect("metadata")
                .kind,
            NodeKind::File
        );
        assert_eq!(
            fs.metadata(node(&fs, "/boot/lib")).expect("metadata").kind,
            NodeKind::Directory
        );
        assert_eq!(
            fs.metadata(node(&fs, "/")).expect("metadata").kind,
            NodeKind::Directory
        );
        // Каталог выведен из той же записи, что и первый его файл, но узлы у
        // них разные.
        assert_ne!(node(&fs, "/boot"), node(&fs, "/boot/init"));
    }

    #[test]
    fn missing_path_is_not_found() {
        let fs = image();
        assert_eq!(fs.lookup(path("/absent")), Err(FileSystemError::NotFound));
        assert_eq!(
            fs.lookup(path("/data/absent")),
            Err(FileSystemError::NotFound)
        );
        assert_eq!(
            fs.lookup(path("/boot/lib/absent")),
            Err(FileSystemError::NotFound)
        );
        assert_eq!(fs.lookup(path("/boot/ini")), Err(FileSystemError::NotFound));
        assert_eq!(
            fs.lookup(path("/data/absent/deep")),
            Err(FileSystemError::NotFound)
        );
    }

    #[test]
    fn descent_through_a_file_is_not_a_directory() {
        let fs = image();
        assert_eq!(
            fs.lookup(path("/data/sample/x")),
            Err(FileSystemError::NotADirectory)
        );
        assert_eq!(
            fs.lookup(path("/boot/init/deep/x")),
            Err(FileSystemError::NotADirectory)
        );
    }

    #[test]
    fn directory_is_found_past_the_names_sorted_before_its_content() {
        let fs = image();
        // "/boot.old" лежит в таблице между "/boot" и "/boot/init".
        assert_eq!(
            fs.metadata(node(&fs, "/boot.old")).expect("metadata").kind,
            NodeKind::File
        );
        assert_eq!(
            fs.metadata(node(&fs, "/boot")).expect("metadata").kind,
            NodeKind::Directory
        );
        assert_eq!(fs.lookup(path("/boots")), Err(FileSystemError::NotFound));
    }

    #[test]
    fn read_at_offset_and_past_the_end() {
        let fs = image();
        let sample = node(&fs, "/data/sample");
        assert_eq!(read(&fs, sample, 0, SAMPLE.len()), SAMPLE);
        assert_eq!(read(&fs, sample, 6, 4), b"from");
        assert_eq!(read(&fs, sample, 6, 64), &SAMPLE[6..]);
        assert_eq!(read(&fs, sample, SAMPLE.len() as u64, 8), b"");
        assert_eq!(read(&fs, sample, u64::MAX, 8), b"");
    }

    #[test]
    fn metadata_reports_the_length_of_a_file_and_the_entries_of_a_directory() {
        let fs = image();
        assert_eq!(
            fs.metadata(node(&fs, "/data/sample"))
                .expect("metadata")
                .size,
            SAMPLE.len() as u64
        );
        // Каталог "/boot" содержит файл "init" и каталог "lib".
        assert_eq!(fs.metadata(node(&fs, "/boot")).expect("metadata").size, 2);
        assert_eq!(
            fs.metadata(node(&fs, "/boot/lib")).expect("metadata").size,
            2
        );
        assert_eq!(fs.metadata(node(&fs, "/")).expect("metadata").size, 3);
    }

    #[test]
    fn directory_lists_files_and_subdirectories_once_each() {
        let fs = image();
        let (entries, batch) = listing(&fs, node(&fs, "/boot"), DirCursor::START, 64);
        assert_eq!(
            entries,
            [
                (String::from("init"), NodeKind::File),
                (String::from("lib"), NodeKind::Directory),
            ]
        );
        assert!(batch.end);

        // Порядок перечисления - порядок таблицы, отличный от порядка имён: "/boot.old"
        // лежит в ней перед содержимым каталога "/boot".
        let (entries, _) = listing(&fs, node(&fs, "/"), DirCursor::START, 64);
        assert_eq!(
            entries,
            [
                (String::from("boot.old"), NodeKind::File),
                (String::from("boot"), NodeKind::Directory),
                (String::from("data"), NodeKind::Directory),
            ]
        );
    }

    #[test]
    fn every_name_is_listed_once_across_batches() {
        let fs = image();
        let root = node(&fs, "/");
        let capacity = ENTRY_HEADER_SIZE + "boot.old".len();

        let mut seen = Vec::new();
        let mut from = DirCursor::START;
        let mut batches = 0;
        loop {
            let (entries, batch) = listing(&fs, root, from, capacity);
            assert_ne!(batch.count, 0, "batch made no progress");
            seen.extend(names(&entries).iter().map(|name| String::from(*name)));
            batches += 1;
            if batch.end {
                break;
            }
            from = batch.next;
        }
        assert_eq!(seen, ["boot.old", "boot", "data"]);
        assert_eq!(batches, 3);
    }

    #[test]
    fn listing_continues_from_the_returned_cursor() {
        let fs = image();
        let root = node(&fs, "/");
        let capacity = 2 * (ENTRY_HEADER_SIZE + "boot.old".len());

        let (entries, first) = listing(&fs, root, DirCursor::START, capacity);
        assert_eq!(names(&entries), ["boot.old", "boot"]);
        assert!(!first.end);
        assert_eq!(first.count, 2);

        let (entries, second) = listing(&fs, root, first.next, capacity);
        assert_eq!(names(&entries), ["data"]);
        assert!(second.end);

        // Позиция за последней записью каталога больше ничего не даёт.
        assert_eq!(
            listing(&fs, root, second.next, capacity).1,
            listed(0, second.next.raw(), true)
        );
    }

    #[test]
    fn record_that_does_not_fit_stops_the_batch_without_progress() {
        let fs = image();
        let root = node(&fs, "/");
        let (entries, batch) = listing(&fs, root, DirCursor::START, ENTRY_HEADER_SIZE);
        assert!(entries.is_empty());
        assert_eq!(batch, listed(0, 0, false));
    }

    #[test]
    fn listing_of_a_nested_directory_ignores_the_neighbours() {
        let fs = image();
        let lib = node(&fs, "/boot/lib");
        // Позиция до первой записи каталога не выносит в перечисление соседей.
        let (entries, batch) = listing(&fs, lib, DirCursor::START, 64);
        assert_eq!(names(&entries), ["hello", "world"]);
        assert!(batch.end);
        assert_eq!(listing(&fs, lib, batch.next, 64).1.count, 0);
    }

    #[test]
    fn file_and_directory_operations_do_not_mix() {
        let mut fs = image();
        let boot = node(&fs, "/boot");
        let sample = node(&fs, "/data/sample");
        assert_eq!(
            fs.read_at(boot, 0, &mut [0; 4]),
            Err(FileSystemError::IsADirectory)
        );
        assert_eq!(fs.backing(boot), Err(FileSystemError::IsADirectory));
        assert_eq!(
            fs.list(sample, DirCursor::START, &mut [0; 16]),
            Err(FileSystemError::NotADirectory)
        );
    }

    #[test]
    fn backing_points_at_the_bytes_of_the_entry() {
        let mut fs = image();
        let Window { offset, size, .. } = fs.backing(node(&fs, "/data/sample")).expect("backing");
        assert_eq!(size, SAMPLE.len() as u64);
        let at = offset as usize;
        assert_eq!(&image_bytes()[at..at + size as usize], SAMPLE);
    }

    #[test]
    fn empty_record_is_a_file_without_a_backing_window() {
        let mut fs = image_from(&[
            ImageRecord {
                path: "/boot/init",
                data: b"init",
            },
            ImageRecord {
                path: "/data/blank",
                data: b"",
            },
            ImageRecord {
                path: "/data/sample",
                data: SAMPLE,
            },
            ImageRecord {
                path: "/data/tail",
                data: b"",
            },
        ])
        .expect("decode image");

        // Пустая запись стоит на том же смещении, что и следующая за ней, а
        // хвостовая - за концом образа. У них нет своих страниц.
        for text in ["/data/blank", "/data/tail"] {
            let empty = node(&fs, text);
            let info = fs.metadata(empty).expect("metadata");
            assert_eq!((info.kind, info.size), (NodeKind::File, 0));
            assert_eq!(read(&fs, empty, 0, 8), b"");
            assert_eq!(fs.backing(empty), Err(FileSystemError::NotSupported));
        }
        let sample = fs.backing(node(&fs, "/data/sample")).expect("backing");
        assert_eq!(fs.entries[1].offset, sample.offset);
    }

    #[test]
    fn mutating_operations_are_refused() {
        let mut fs = image();
        let sample = node(&fs, "/data/sample");
        assert_eq!(
            fs.create(path("/data/fresh"), NodeKind::File),
            Err(FileSystemError::ReadOnly)
        );
        assert_eq!(
            fs.unlink(path("/data/sample"), NodeKind::File),
            Err(FileSystemError::ReadOnly)
        );
        assert_eq!(
            fs.rename(path("/data/sample"), path("/data/sample.old")),
            Err(FileSystemError::ReadOnly)
        );
        assert_eq!(fs.write_at(sample, 0, b"x"), Err(FileSystemError::ReadOnly));
        assert_eq!(fs.append(sample, b"x"), Err(FileSystemError::ReadOnly));
        assert_eq!(fs.set_size(sample, 0), Err(FileSystemError::ReadOnly));
        // Отказ ничего не тронул.
        assert_eq!(read(&fs, sample, 0, SAMPLE.len()), SAMPLE);
    }

    #[test]
    fn changing_a_path_that_does_not_resolve_reports_the_path() {
        let mut fs = image();
        assert_eq!(
            fs.unlink(path("/data/absent"), NodeKind::File),
            Err(FileSystemError::NotFound)
        );
        assert_eq!(
            fs.unlink(path("/data/sample/x"), NodeKind::File),
            Err(FileSystemError::NotADirectory)
        );
        assert_eq!(
            fs.create(path("/absent/fresh"), NodeKind::File),
            Err(FileSystemError::NotFound)
        );
        assert_eq!(
            fs.create(path("/data/sample/fresh"), NodeKind::File),
            Err(FileSystemError::NotADirectory)
        );
        // Занятое имя - причина пути. Заведение имени его не меняет.
        assert_eq!(
            fs.create(path("/data/sample"), NodeKind::File),
            Err(FileSystemError::AlreadyExists)
        );
        assert_eq!(
            fs.rename(path("/data/absent"), path("/data/fresh")),
            Err(FileSystemError::NotFound)
        );
        assert_eq!(
            fs.rename(path("/data/sample"), path("/absent/fresh")),
            Err(FileSystemError::NotFound)
        );
        // Замещаемый каталог - причина вида узла.
        assert_eq!(
            fs.rename(path("/data/sample"), path("/boot")),
            Err(FileSystemError::IsADirectory)
        );
    }

    #[test]
    fn unlink_of_a_kind_that_does_not_match_reports_the_kind() {
        let mut fs = image();
        assert_eq!(
            fs.unlink(path("/boot"), NodeKind::File),
            Err(FileSystemError::IsADirectory)
        );
        assert_eq!(
            fs.unlink(path("/data/sample"), NodeKind::Directory),
            Err(FileSystemError::NotADirectory)
        );
        assert_eq!(
            fs.unlink(path("/boot"), NodeKind::Directory),
            Err(FileSystemError::ReadOnly)
        );
        assert_eq!(
            fs.unlink(path("/data/sample"), NodeKind::File),
            Err(FileSystemError::ReadOnly)
        );
        assert_eq!(
            fs.unlink(path("/data/absent"), NodeKind::Directory),
            Err(FileSystemError::NotFound)
        );
    }

    #[test]
    fn changing_the_name_of_the_root_is_rejected() {
        let mut fs = image();
        assert_eq!(
            fs.create(Path::ROOT, NodeKind::Directory),
            Err(FileSystemError::InvalidPath)
        );
        assert_eq!(
            fs.unlink(Path::ROOT, NodeKind::Directory),
            Err(FileSystemError::InvalidPath)
        );
        assert_eq!(
            fs.rename(Path::ROOT, path("/fresh")),
            Err(FileSystemError::InvalidPath)
        );
        assert_eq!(
            fs.rename(path("/data/sample"), Path::ROOT),
            Err(FileSystemError::InvalidPath)
        );
    }

    #[test]
    fn changing_a_directory_reports_its_kind() {
        let mut fs = image();
        let boot = node(&fs, "/boot");
        // Вид узла - более конкретная причина отказа, чем режим монтирования.
        assert_eq!(
            fs.write_at(boot, 0, b"x"),
            Err(FileSystemError::IsADirectory)
        );
        assert_eq!(fs.append(boot, b"x"), Err(FileSystemError::IsADirectory));
        assert_eq!(fs.set_size(boot, 0), Err(FileSystemError::IsADirectory));
    }

    #[test]
    fn changing_an_unknown_node_is_not_found() {
        let mut fs = image();
        let absent = NodeId::from_raw(u32::MAX);
        assert_eq!(fs.write_at(absent, 0, b"x"), Err(FileSystemError::NotFound));
        assert_eq!(fs.append(absent, b"x"), Err(FileSystemError::NotFound));
        assert_eq!(fs.set_size(absent, 0), Err(FileSystemError::NotFound));
    }

    #[test]
    fn table_with_a_path_outside_the_contract_is_refused() {
        let relative = image_from(&[ImageRecord {
            path: "boot/init",
            data: b"init",
        }]);
        assert_eq!(
            relative.err(),
            Some(ImageFileSystemError::EntryPath { index: 0 })
        );
    }

    #[test]
    fn record_matching_a_path_whole_shadows_the_directory_of_the_same_name() {
        let fs = image_from(&[
            ImageRecord {
                path: "/a",
                data: b"file",
            },
            ImageRecord {
                path: "/a/deep",
                data: b"deep",
            },
        ])
        .expect("decode image");

        // Запись, совпавшая с путём целиком, - файл. Перечисление не содержит каталога с тем
        // же именем, а вложенная запись достаётся по своему пути.
        let info = fs.metadata(node(&fs, "/a")).expect("metadata");
        assert_eq!((info.kind, info.size), (NodeKind::File, 4));
        assert_eq!(read(&fs, node(&fs, "/a/deep"), 0, 8), b"deep");
        assert_eq!(
            listing(&fs, node(&fs, "/"), DirCursor::START, 64).0,
            [(String::from("a"), NodeKind::File)]
        );
    }
}
