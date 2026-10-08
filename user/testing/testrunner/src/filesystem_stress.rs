//! У воспроизводимого stress-прогона изменяемого дерева операции контракта `FileSystem` идут
//! в процесс ramfs по IPC, а их итоги сверяются с моделью дерева, которую прогон держит у себя.
//!
//! Каждый seed работает на своём окне. Монтированию прогона выдана сессия, суженная до каталога
//! этого seed, и корнем для него становится сам каталог. Имена, оставленные соседними seed,
//! в окно не попадают, и последовательность операций одного seed от них не зависит. Окно
//! снимается вместе с монтированием: арена узлов у процесса файловой системы одна на все seed.

use alloc::{collections::BTreeMap, format, string::String, vec, vec::Vec};

use filesystem_abi::{
    APPEND_OFFSET, AccessMode, DirCursor, DirEntry, FileSystemError, NodeDescriptor, NodeKind,
    OpenFlags, file_system,
};
use kernel_objects::{Mapping, MemoryRegion, SessionTransport};
use runtime::UserMemFlags;

use crate::{
    filesystem::{STRESS_VIEW, connect, mount_ram_window, path, remove_ram_directory, session_for},
    stress::{SplitMix64, StressPlan, log_pass, log_seed},
};

const STRESS_MOUNT: &str = "/stress";

const BUFFER_SIZE: u32 = 4096;

const PAYLOAD_MAX: usize = 192;

const OFFSET_MAX: u64 = 512;

const SIZE_MAX: u64 = 1024;

const DESCRIPTORS_MAX: usize = 8;

const DIRECTORIES: [&str; 2] = ["/d0", "/d1"];

const FILE_NAMES: [&str; 4] = ["f0", "f1", "f2", "f3"];

const LIST_BATCHES_MAX: usize = 16;

type Step = Result<(), Mismatch>;

#[derive(Debug)]
enum Mismatch {
    Call(&'static str),
    Refused(&'static str, FileSystemError),
    Model(&'static str),
}

struct Session {
    client: file_system::Client<SessionTransport>,
    buffer: Mapping,
    _region: MemoryRegion,
}

struct Descriptor {
    fd: NodeDescriptor,
    mode: AccessMode,
    path: Option<String>,
    frozen: Vec<u8>,
}

struct Run {
    session: Session,
    files: BTreeMap<String, Vec<u8>>,
    descriptors: Vec<Descriptor>,
    random: SplitMix64,
}

impl Mismatch {
    fn refused(operation: &'static str) -> impl FnOnce(FileSystemError) -> Self {
        move |error| Self::Refused(operation, error)
    }
}

impl core::fmt::Display for Mismatch {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Call(operation) => write!(formatter, "{operation} call failed"),
            Self::Refused(operation, error) => {
                write!(formatter, "{operation} refused with {error:?}")
            }
            Self::Model(reason) => write!(formatter, "{reason}"),
        }
    }
}

impl Session {
    fn open() -> Result<Self, Mismatch> {
        let client = connect(session_for(STRESS_MOUNT));
        let granted = client
            .acquire_buffer(BUFFER_SIZE)
            .map_err(|_| Mismatch::Call("acquire_buffer"))?
            .map_err(Mismatch::refused("acquire_buffer"))?;
        let region = MemoryRegion::from(granted);
        let buffer = region
            .map_full(UserMemFlags::ReadWrite)
            .map_err(|_| Mismatch::Model("data buffer is not mapped"))?;
        Ok(Self {
            client,
            buffer,
            _region: region,
        })
    }

    fn put(&self, bytes: &[u8]) -> Step {
        self.buffer
            .copy_in(bytes)
            .map_err(|_| Mismatch::Model("payload does not fit the buffer"))
    }

    fn take(&self, len: usize) -> Result<Vec<u8>, Mismatch> {
        let mut bytes = vec![0; len];
        self.buffer
            .copy_out(&mut bytes)
            .map_err(|_| Mismatch::Model("buffer head is not readable"))?;
        Ok(bytes)
    }
}

impl Run {
    fn new(seed: u64) -> Result<Self, Mismatch> {
        let run = Self {
            session: Session::open()?,
            files: BTreeMap::new(),
            descriptors: Vec::new(),
            random: SplitMix64::new(seed),
        };
        for directory in DIRECTORIES {
            run.session
                .client
                .open(
                    path(directory),
                    AccessMode::Read,
                    OpenFlags::CREATE | OpenFlags::DIRECTORY,
                )
                .map_err(|_| Mismatch::Call("open"))?
                .map_err(Mismatch::refused("open of a directory"))?;
        }
        Ok(run)
    }

    fn step(&mut self) -> (&'static str, Step) {
        match self.choice(15) {
            0 => ("open_create", self.open_create()),
            1 => ("open_exclusive", self.open_exclusive()),
            2 => ("open_existing", self.open_existing()),
            3 => ("write", self.write()),
            4 => ("append", self.append()),
            5 => ("read", self.read()),
            6 => ("set_size", self.set_size()),
            7 => ("stat", self.stat()),
            8 => ("unlink", self.unlink()),
            9 => ("rename", self.rename()),
            10 => ("list", self.list()),
            11 => ("close", self.close()),
            12 => ("renew_session", self.renew_session()),
            13 => (
                "write_through_a_read_only",
                self.write_through_a_read_only(),
            ),
            _ => ("read_over_the_buffer", self.read_over_the_buffer()),
        }
    }

    fn open_create(&mut self) -> Step {
        let text = self.file_path();
        let expected = self.files.get(&text).map_or(0, Vec::len) as u64;
        let opened = self
            .session
            .client
            .open(path(&text), AccessMode::ReadWrite, OpenFlags::CREATE)
            .map_err(|_| Mismatch::Call("open"))?
            .map_err(Mismatch::refused("open with CREATE"))?;
        expect(opened.kind == NodeKind::File, "opened node is not a file")?;
        expect(
            opened.size == expected,
            "opened size differs from the model",
        )?;
        self.files.entry(text.clone()).or_default();
        self.keep(Descriptor {
            fd: opened.fd,
            mode: AccessMode::ReadWrite,
            path: Some(text),
            frozen: Vec::new(),
        })
    }

    fn open_exclusive(&mut self) -> Step {
        let text = self.file_path();
        let known = self.files.contains_key(&text);
        let opened = self
            .session
            .client
            .open(
                path(&text),
                AccessMode::ReadWrite,
                OpenFlags::CREATE | OpenFlags::EXCLUSIVE,
            )
            .map_err(|_| Mismatch::Call("open"))?;
        if known {
            return expect(
                opened == Err(FileSystemError::AlreadyExists),
                "occupied name is opened exclusively",
            );
        }
        let opened = opened.map_err(Mismatch::refused("exclusive open of a free name"))?;
        expect(opened.size == 0, "created node is not empty")?;
        self.files.insert(text.clone(), Vec::new());
        self.keep(Descriptor {
            fd: opened.fd,
            mode: AccessMode::ReadWrite,
            path: Some(text),
            frozen: Vec::new(),
        })
    }

    fn open_existing(&mut self) -> Step {
        let text = self.file_path();
        let expected = self.files.get(&text).map(|data| data.len() as u64);
        let opened = self
            .session
            .client
            .open(path(&text), AccessMode::Read, OpenFlags::NONE)
            .map_err(|_| Mismatch::Call("open"))?;
        let Some(size) = expected else {
            return expect(
                opened == Err(FileSystemError::NotFound),
                "missing name is opened without CREATE",
            );
        };
        let opened = opened.map_err(Mismatch::refused("open of a known name"))?;
        expect(opened.size == size, "opened size differs from the model")?;
        self.keep(Descriptor {
            fd: opened.fd,
            mode: AccessMode::Read,
            path: Some(text),
            frozen: Vec::new(),
        })
    }

    fn write(&mut self) -> Step {
        let Some(index) = self.writable_descriptor() else {
            return Ok(());
        };
        let fd = self.descriptors[index].fd;
        let text = self.named_path(index)?;
        let payload = self.payload();
        let offset = self.random.next() % (OFFSET_MAX + 1);
        self.session.put(&payload)?;
        let written = self
            .session
            .client
            .write(fd, offset, payload.len() as u32)
            .map_err(|_| Mismatch::Call("write"))?
            .map_err(Mismatch::refused("write"))?;
        expect(written.offset == offset, "write landed at another offset")?;
        expect(
            written.count as usize == payload.len(),
            "write took another number of bytes",
        )?;
        let file = self.file_mut(&text)?;
        write_into(file, offset as usize, &payload);
        Ok(())
    }

    fn append(&mut self) -> Step {
        let Some(index) = self.writable_descriptor() else {
            return Ok(());
        };
        let fd = self.descriptors[index].fd;
        let text = self.named_path(index)?;
        let payload = self.payload();
        let end = self.file_mut(&text)?.len() as u64;
        self.session.put(&payload)?;
        let written = self
            .session
            .client
            .write(fd, APPEND_OFFSET, payload.len() as u32)
            .map_err(|_| Mismatch::Call("write"))?
            .map_err(Mismatch::refused("append"))?;
        expect(written.offset == end, "append landed before the end")?;
        expect(
            written.count as usize == payload.len(),
            "append took another number of bytes",
        )?;
        let file = self.file_mut(&text)?;
        write_into(file, end as usize, &payload);
        Ok(())
    }

    fn read(&mut self) -> Step {
        let Some(index) = self.any_descriptor() else {
            return Ok(());
        };
        let fd = self.descriptors[index].fd;
        let content = self.content(index);
        let offset = self.random.next() % (OFFSET_MAX + 1);
        let len = self.choice(PAYLOAD_MAX + 1);
        let count = self
            .session
            .client
            .read(fd, offset, len as u32)
            .map_err(|_| Mismatch::Call("read"))?
            .map_err(Mismatch::refused("read"))?;
        let tail = content.get(offset as usize..).unwrap_or_default();
        let expected = tail.len().min(len);
        expect(count as usize == expected, "read took another byte count")?;
        let bytes = self.session.take(expected)?;
        expect(
            bytes == tail[..expected],
            "read bytes differ from the model",
        )
    }

    fn set_size(&mut self) -> Step {
        let Some(index) = self.writable_descriptor() else {
            return Ok(());
        };
        let fd = self.descriptors[index].fd;
        let text = self.named_path(index)?;
        let size = self.random.next() % (SIZE_MAX + 1);
        self.session
            .client
            .set_size(fd, size)
            .map_err(|_| Mismatch::Call("set_size"))?
            .map_err(Mismatch::refused("set_size"))?;
        self.file_mut(&text)?.resize(size as usize, 0);
        Ok(())
    }

    fn stat(&mut self) -> Step {
        let Some(index) = self.any_descriptor() else {
            return Ok(());
        };
        let fd = self.descriptors[index].fd;
        let content = self.content(index);
        let info = self
            .session
            .client
            .stat(fd)
            .map_err(|_| Mismatch::Call("stat"))?
            .map_err(Mismatch::refused("stat"))?;
        expect(
            info.kind == NodeKind::File,
            "node under the descriptor is not a file",
        )?;
        expect(
            info.size == content.len() as u64,
            "stat size differs from the model",
        )
    }

    fn unlink(&mut self) -> Step {
        let text = self.file_path();
        let known = self.files.remove(&text);
        let removed = self
            .session
            .client
            .unlink(path(&text), NodeKind::File)
            .map_err(|_| Mismatch::Call("unlink"))?;
        let Some(content) = known else {
            return expect(
                removed == Err(FileSystemError::NotFound),
                "missing name is unlinked",
            );
        };
        removed.map_err(Mismatch::refused("unlink of a known name"))?;
        freeze(&mut self.descriptors, &text, &content);
        Ok(())
    }

    fn rename(&mut self) -> Step {
        let from = self.file_path();
        let to = self.file_path();
        let renamed = self
            .session
            .client
            .rename(path(&from), path(&to))
            .map_err(|_| Mismatch::Call("rename"))?;
        let Some(content) = self.files.remove(&from) else {
            return expect(
                renamed == Err(FileSystemError::NotFound),
                "missing name is renamed",
            );
        };
        renamed.map_err(Mismatch::refused("rename of a known name"))?;
        if from == to {
            self.files.insert(to, content);
            return Ok(());
        }
        if let Some(replaced) = self.files.remove(&to) {
            freeze(&mut self.descriptors, &to, &replaced);
        }
        for descriptor in &mut self.descriptors {
            if descriptor.path.as_deref() == Some(from.as_str()) {
                descriptor.path = Some(to.clone());
            }
        }
        self.files.insert(to, content);
        Ok(())
    }

    fn list(&mut self) -> Step {
        let text = self.directory_path();
        self.assert_listing(&text)
    }

    fn close(&mut self) -> Step {
        if self.descriptors.is_empty() {
            return Ok(());
        }
        let index = self.choice(self.descriptors.len());
        let descriptor = self.descriptors.remove(index);
        self.session
            .client
            .close(descriptor.fd)
            .map_err(|_| Mismatch::Call("close"))?
            .map_err(Mismatch::refused("close"))?;
        let stale = self
            .session
            .client
            .stat(descriptor.fd)
            .map_err(|_| Mismatch::Call("stat"))?;
        expect(
            stale == Err(FileSystemError::BadDescriptor),
            "closed descriptor still reaches its node",
        )
    }

    fn renew_session(&mut self) -> Step {
        self.descriptors.clear();
        self.session = Session::open()?;
        let known: Vec<String> = self.files.keys().cloned().collect();
        for text in known {
            let size = self.files.get(&text).map_or(0, Vec::len) as u64;
            let opened = self
                .session
                .client
                .open(path(&text), AccessMode::Read, OpenFlags::NONE)
                .map_err(|_| Mismatch::Call("open"))?
                .map_err(Mismatch::refused(
                    "open of a node that outlived the session",
                ))?;
            expect(opened.size == size, "size is gone with the session")?;
            self.session
                .client
                .close(opened.fd)
                .map_err(|_| Mismatch::Call("close"))?
                .map_err(Mismatch::refused("close"))?;
        }
        Ok(())
    }

    fn write_through_a_read_only(&mut self) -> Step {
        let Some(index) = self.descriptor_matching(|mode| !mode.writable()) else {
            return Ok(());
        };
        let fd = self.descriptors[index].fd;
        let refused = self
            .session
            .client
            .write(fd, 0, 1)
            .map_err(|_| Mismatch::Call("write"))?;
        expect(
            refused == Err(FileSystemError::WrongMode),
            "read-only descriptor takes a write",
        )
    }

    fn read_over_the_buffer(&mut self) -> Step {
        let Some(index) = self.any_descriptor() else {
            return Ok(());
        };
        let fd = self.descriptors[index].fd;
        let refused = self
            .session
            .client
            .read(fd, 0, BUFFER_SIZE + 1)
            .map_err(|_| Mismatch::Call("read"))?;
        expect(
            refused == Err(FileSystemError::BufferTooSmall),
            "read past the buffer is served",
        )
    }

    fn verify(&mut self) -> Step {
        let known: Vec<String> = self.files.keys().cloned().collect();
        for text in known {
            let content = self.files.get(&text).cloned().unwrap_or_default();
            expect(
                content.len() < BUFFER_SIZE as usize,
                "file outgrew the data buffer of the run",
            )?;
            let opened = self
                .session
                .client
                .open(path(&text), AccessMode::Read, OpenFlags::NONE)
                .map_err(|_| Mismatch::Call("open"))?
                .map_err(Mismatch::refused("open of a node of the tree"))?;
            expect(
                opened.size == content.len() as u64,
                "size differs from the model",
            )?;
            let count = self
                .session
                .client
                .read(opened.fd, 0, content.len() as u32)
                .map_err(|_| Mismatch::Call("read"))?
                .map_err(Mismatch::refused("read"))?;
            expect(
                count as usize == content.len(),
                "read took another byte count",
            )?;
            let bytes = self.session.take(content.len())?;
            expect(bytes == content, "bytes differ from the model")?;
            self.session
                .client
                .close(opened.fd)
                .map_err(|_| Mismatch::Call("close"))?
                .map_err(Mismatch::refused("close"))?;
        }
        self.assert_listing("/")?;
        for directory in DIRECTORIES {
            self.assert_listing(directory)?;
        }
        Ok(())
    }

    fn clear(&mut self) -> Step {
        // Окно сида не сохраняется после его прогона. Узлы дерева возвращаются в арену, общую
        // у всех сидов.
        let names: Vec<String> = self.files.keys().cloned().collect();
        let files = names.iter().map(|text| (text.as_str(), NodeKind::File));
        let directories = DIRECTORIES.map(|text| (text, NodeKind::Directory));
        for (text, expected) in files.chain(directories) {
            self.session
                .client
                .unlink(path(text), expected)
                .map_err(|_| Mismatch::Call("unlink"))?
                .map_err(Mismatch::refused("unlink of the window"))?;
        }
        Ok(())
    }

    fn assert_listing(&mut self, text: &str) -> Step {
        let opened = self
            .session
            .client
            .open(path(text), AccessMode::Read, OpenFlags::DIRECTORY)
            .map_err(|_| Mismatch::Call("open"))?
            .map_err(Mismatch::refused("open of a directory"))?;
        let mut listed = self.walk(opened.fd)?;
        self.session
            .client
            .close(opened.fd)
            .map_err(|_| Mismatch::Call("close"))?
            .map_err(Mismatch::refused("close"))?;
        let mut expected = self.children(text);
        listed.sort_unstable();
        expected.sort_unstable();
        expect(listed == expected, "listing differs from the model")
    }

    fn walk(&self, fd: NodeDescriptor) -> Result<Vec<String>, Mismatch> {
        let mut names = Vec::new();
        let mut from = DirCursor::START;
        for _ in 0..LIST_BATCHES_MAX {
            let listed = self
                .session
                .client
                .list(fd, from)
                .map_err(|_| Mismatch::Call("list"))?
                .map_err(Mismatch::refused("list"))?;
            let bytes = self.session.take(BUFFER_SIZE as usize)?;
            let mut offset = 0;
            for _ in 0..listed.count {
                let decoded = DirEntry::decode(&bytes[offset..])
                    .map_err(|_| Mismatch::Model("directory record is malformed"))?;
                names.push(String::from(decoded.entry.name));
                offset += decoded.len;
            }
            if listed.end {
                return Ok(names);
            }
            // Незавершённый обход обязан продвинуться. Буфера прогона хватает на любую запись.
            if listed.count == 0 || listed.next.raw() <= from.raw() {
                return Err(Mismatch::Model("listing did not advance"));
            }
            from = listed.next;
        }
        Err(Mismatch::Model("listing did not converge"))
    }

    fn writable_descriptor(&mut self) -> Option<usize> {
        let writable: Vec<usize> = self
            .descriptors
            .iter()
            .enumerate()
            .filter_map(|(index, descriptor)| {
                (descriptor.mode.writable() && descriptor.path.is_some()).then_some(index)
            })
            .collect();
        writable.get(self.choice(writable.len().max(1))).copied()
    }

    fn any_descriptor(&mut self) -> Option<usize> {
        if self.descriptors.is_empty() {
            return None;
        }
        Some(self.choice(self.descriptors.len()))
    }

    fn descriptor_matching(&mut self, allowed: impl Fn(AccessMode) -> bool) -> Option<usize> {
        let matching: Vec<usize> = self
            .descriptors
            .iter()
            .enumerate()
            .filter_map(|(index, descriptor)| allowed(descriptor.mode).then_some(index))
            .collect();
        matching.get(self.choice(matching.len().max(1))).copied()
    }

    fn named_path(&self, index: usize) -> Result<String, Mismatch> {
        self.descriptors[index]
            .path
            .clone()
            .ok_or(Mismatch::Model("descriptor lost its name"))
    }

    fn file_mut(&mut self, text: &str) -> Result<&mut Vec<u8>, Mismatch> {
        self.files
            .get_mut(text)
            .ok_or(Mismatch::Model("file left the model"))
    }

    fn content(&self, index: usize) -> Vec<u8> {
        let descriptor = &self.descriptors[index];
        match &descriptor.path {
            Some(text) => self.files.get(text).cloned().unwrap_or_default(),
            None => descriptor.frozen.clone(),
        }
    }

    fn keep(&mut self, descriptor: Descriptor) -> Step {
        if self.descriptors.len() >= DESCRIPTORS_MAX {
            let oldest = self.descriptors.remove(0);
            self.session
                .client
                .close(oldest.fd)
                .map_err(|_| Mismatch::Call("close"))?
                .map_err(Mismatch::refused("close"))?;
        }
        self.descriptors.push(descriptor);
        Ok(())
    }

    fn children(&self, directory: &str) -> Vec<String> {
        let mut names: Vec<String> = self
            .files
            .keys()
            .filter_map(|key| {
                let (parent, name) = split_last(key)?;
                (parent == directory).then(|| String::from(name))
            })
            .collect();
        if directory == "/" {
            names.extend(DIRECTORIES.iter().filter_map(|text| {
                let (_, name) = split_last(text)?;
                Some(String::from(name))
            }));
        }
        names
    }

    fn file_path(&mut self) -> String {
        let name = FILE_NAMES[self.choice(FILE_NAMES.len())];
        let directory = self.choice(DIRECTORIES.len() + 1);
        match DIRECTORIES.get(directory) {
            Some(parent) => format!("{parent}/{name}"),
            None => format!("/{name}"),
        }
    }

    fn directory_path(&mut self) -> String {
        let index = self.choice(DIRECTORIES.len() + 1);
        DIRECTORIES
            .get(index)
            .map_or_else(|| String::from("/"), |text| String::from(*text))
    }

    fn payload(&mut self) -> Vec<u8> {
        let len = self.choice(PAYLOAD_MAX + 1);
        (0..len).map(|_| (self.random.next() >> 24) as u8).collect()
    }

    fn choice(&mut self, count: usize) -> usize {
        (self.random.next() % count as u64) as usize
    }
}

#[test_harness::test(stress)]
fn filesystem_ramfs_stress() {
    let plan = StressPlan::load();
    for seed in plan.seeds() {
        log_seed(seed, plan.steps);
        run_seed(seed, plan.steps);
        log_pass(seed);
    }
}

fn run_seed(seed: u64, steps: u64) {
    let scope = scope_of(seed);
    let mounted = mount_ram_window(STRESS_VIEW, &scope);
    let mut run = Run::new(seed).expect("session of the run");
    for step in 0..steps {
        let (operation, result) = run.step();
        if let Err(mismatch) = result {
            report(seed, step, operation, &mismatch);
        }
    }
    if let Err(mismatch) = run.verify() {
        report(seed, steps, "verify", &mismatch);
    }
    if let Err(mismatch) = run.clear() {
        report(seed, steps, "clear", &mismatch);
    }
    drop(run);
    drop(mounted);
    remove_ram_directory(&scope);
}

fn scope_of(seed: u64) -> String {
    format!("/seed-{seed}")
}

fn report(seed: u64, step: u64, operation: &'static str, mismatch: &Mismatch) -> ! {
    test_harness::macros::fail(
        format_args!("stress seed={seed} step={step} operation={operation}: {mismatch}"),
        file!(),
        line!(),
    )
}

fn expect(condition: bool, reason: &'static str) -> Step {
    if condition {
        return Ok(());
    }
    Err(Mismatch::Model(reason))
}

fn freeze(descriptors: &mut [Descriptor], text: &str, content: &[u8]) {
    for descriptor in descriptors {
        if descriptor.path.as_deref() == Some(text) {
            descriptor.path = None;
            descriptor.frozen = Vec::from(content);
        }
    }
}

fn split_last(path: &str) -> Option<(&str, &str)> {
    if path == "/" {
        return None;
    }
    let cut = path.rfind('/')?;
    let parent = if cut == 0 { "/" } else { &path[..cut] };
    Some((parent, &path[cut + 1..]))
}

fn write_into(file: &mut Vec<u8>, start: usize, data: &[u8]) {
    let end = start + data.len();
    if file.len() < end {
        file.resize(end, 0);
    }
    file[start..end].copy_from_slice(data);
}
