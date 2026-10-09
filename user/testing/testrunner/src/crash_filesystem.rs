#![no_std]
#![no_main]

extern crate alloc;

use component_startup::component_manifest;
use filesystem::{ByteStore, FileSystemBackend, NodeId, RamFileSystem, Window};
use filesystem_abi::{DirCursor, FileSystemError, Listed, NodeInfo, NodeKind, Path, file_system};
use filesystem_server::serve;
use runtime::{FAILED_EXIT_CODE, exit};
use syscall::{Handle, USER_VA_END, kind};

component_manifest! {
    exports: [file_system::Protocol],
}

struct CrashingFileSystem {
    inner: RamFileSystem,
}

impl FileSystemBackend for CrashingFileSystem {
    fn is_mutable(&self) -> bool {
        self.inner.is_mutable()
    }

    fn lookup(&self, path: Path<'_>) -> Result<NodeId, FileSystemError> {
        self.inner.lookup(path)
    }

    fn create(&mut self, path: Path<'_>, kind: NodeKind) -> Result<NodeId, FileSystemError> {
        self.inner.create(path, kind)
    }

    fn unlink(
        &mut self,
        stores: &mut dyn ByteStore,
        path: Path<'_>,
        expected: NodeKind,
    ) -> Result<(), FileSystemError> {
        self.inner.unlink(stores, path, expected)
    }

    fn rename(
        &mut self,
        _stores: &mut dyn ByteStore,
        _from: Path<'_>,
        _to: Path<'_>,
    ) -> Result<(), FileSystemError> {
        kill_process_by_fault()
    }

    fn retain(&mut self, node: NodeId) -> Result<(), FileSystemError> {
        self.inner.retain(node)
    }

    fn release(&mut self, stores: &mut dyn ByteStore, node: NodeId) {
        self.inner.release(stores, node);
    }

    fn metadata(&self, node: NodeId) -> Result<NodeInfo, FileSystemError> {
        self.inner.metadata(node)
    }

    fn read_at(
        &self,
        stores: &dyn ByteStore,
        node: NodeId,
        offset: u64,
        out: &mut [u8],
    ) -> Result<usize, FileSystemError> {
        self.inner.read_at(stores, node, offset, out)
    }

    fn write_at(
        &mut self,
        stores: &mut dyn ByteStore,
        node: NodeId,
        offset: u64,
        data: &[u8],
    ) -> Result<usize, FileSystemError> {
        self.inner.write_at(stores, node, offset, data)
    }

    fn append(
        &mut self,
        stores: &mut dyn ByteStore,
        node: NodeId,
        data: &[u8],
    ) -> Result<u64, FileSystemError> {
        self.inner.append(stores, node, data)
    }

    fn set_size(
        &mut self,
        stores: &mut dyn ByteStore,
        node: NodeId,
        size: u64,
    ) -> Result<(), FileSystemError> {
        self.inner.set_size(stores, node, size)
    }

    fn list(
        &self,
        node: NodeId,
        from: DirCursor,
        out: &mut [u8],
    ) -> Result<Listed, FileSystemError> {
        self.inner.list(node, from, out)
    }

    fn backing(
        &mut self,
        stores: &mut dyn ByteStore,
        node: NodeId,
    ) -> Result<Window, FileSystemError> {
        self.inner.backing(stores, node)
    }

    fn pin(&mut self, node: NodeId) -> Result<(), FileSystemError> {
        self.inner.pin(node)
    }
}

#[allow(unsafe_code)]
fn kill_process_by_fault() -> ! {
    let unmapped = USER_VA_END as *const u64;
    // SAFETY: адрес намеренно недоступен из EL0 - фикстура проверяет завершение процесса ядром.
    unsafe {
        unmapped.read_volatile();
    }
    exit(FAILED_EXIT_CODE)
}

#[program_entry::component]
fn run(parent: Handle<kind::Port>) -> filesystem_server::Result<()> {
    let backend = |_| {
        Ok(CrashingFileSystem {
            inner: RamFileSystem::new(),
        })
    };
    let Err(error) = serve(parent, None, backend);
    Err(error)
}
